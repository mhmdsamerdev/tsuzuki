-- Required mpv script for Tsuzuki: its only view of what you watch.
-- Logs duration, last position and why playback ended (eof, stop, quit, ...).
-- Passive: one file per played path (streams: per title), written atomically
-- (temp file + rename).
-- Nothing is read back and no socket is opened.

local last_time_pos = nil
local current_duration = nil
local current_path = nil
local current_title = nil
local log_dir_created = false

-- djb2 with plain integer arithmetic; bitwise operators and bit libraries
-- differ across the Lua versions mpv builds ship with.
local function hash_path(str)
    local hash = 5381
    for i = 1, #str do
        hash = (hash * 33 + str:byte(i)) % 4294967296
    end
    return string.format("%08x", hash)
end

local function shell_quote(s)
    return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function log_dir()
    local base = os.getenv("XDG_DATA_HOME")
    if base == nil or base == "" then
        local home = os.getenv("HOME")
        if home == nil or home == "" then
            return nil
        end
        base = home .. "/.local/share"
    end
    return base .. "/tsuzuki/mpv-log"
end

-- Stream URLs change on every launch, so streams are keyed by their title.
local function is_url(path)
    return path:match("^%a[%w+.-]*://") ~= nil
end

local function write_entry(path, title, duration, position, reason)
    local dir = log_dir()
    if dir == nil then
        return
    end
    if not log_dir_created then
        os.execute("mkdir -p " .. shell_quote(dir))
        log_dir_created = true
    end

    local name = hash_path(title and ("title:" .. title) or path)
    local tmp_path = dir .. "/." .. name .. ".tmp"
    local final_path = dir .. "/" .. name

    local f = io.open(tmp_path, "w")
    if f == nil then
        return
    end
    f:write("# " .. path .. "\n")
    if title then
        f:write("title=" .. title .. "\n")
    end
    f:write("duration=" .. (duration and string.format("%.6f", duration) or "") .. "\n")
    f:write("position=" .. (position and string.format("%.6f", position) or "") .. "\n")
    f:write("reason=" .. (reason or "") .. "\n")
    f:close()

    os.rename(tmp_path, final_path)
end

mp.observe_property("time-pos", "number", function(_, value)
    if value ~= nil then
        last_time_pos = value
    end
end)

mp.observe_property("duration", "number", function(_, value)
    if value ~= nil then
        current_duration = value
    end
end)

mp.register_event("start-file", function()
    last_time_pos = nil
    current_duration = nil
    current_path = nil
    current_title = nil
end)

mp.register_event("file-loaded", function()
    current_path = mp.get_property("path")
    local title = mp.get_property("media-title")
    if current_path ~= nil and is_url(current_path) and title ~= nil and title ~= "" then
        current_title = title:gsub("[\r\n]+", " ")
    end
end)

mp.register_event("end-file", function(event)
    if current_path ~= nil then
        write_entry(current_path, current_title, current_duration, last_time_pos, event.reason)
    end
end)
