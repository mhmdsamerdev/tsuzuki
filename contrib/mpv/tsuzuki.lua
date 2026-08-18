-- tsuzuki.lua: optional mpv companion script for Tsuzuki.
-- Installing this is never required -- Tsuzuki works from mpv's own
-- watch_later files without it. When present, it gives Tsuzuki a direct,
-- unambiguous signal per file (duration, last position, and *why*
-- playback ended -- genuinely reached the end vs. skipped away vs. quit)
-- instead of inferring those from watch_later state after the fact.
--
-- Purely passive: writes one small text file per played source path to
-- its own log directory, atomically (temp file + rename), and never reads
-- anything back or opens a socket. mpv loads this identically for every
-- invocation regardless of what launched it (Tsuzuki, a terminal, a file
-- manager) -- the same launcher-independence Tsuzuki's tracking already
-- relies on for watch_later itself.

local last_time_pos = nil
local current_duration = nil
local current_path = nil
local log_dir_created = false

-- djb2, pure integer arithmetic only (+, *, %) -- deliberately avoids
-- Lua 5.3+-only bitwise operators and the 5.1/5.2 bit/bit32 libraries,
-- since mpv ships with different Lua versions (often LuaJIT) depending on
-- build; this needs to work identically on all of them.
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

local function write_entry(path, duration, position, reason)
    local dir = log_dir()
    if dir == nil then
        return
    end
    if not log_dir_created then
        os.execute("mkdir -p " .. shell_quote(dir))
        log_dir_created = true
    end

    local name = hash_path(path)
    local tmp_path = dir .. "/." .. name .. ".tmp"
    local final_path = dir .. "/" .. name

    local f = io.open(tmp_path, "w")
    if f == nil then
        return
    end
    f:write("# " .. path .. "\n")
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
end)

mp.register_event("file-loaded", function()
    current_path = mp.get_property("path")
end)

mp.register_event("end-file", function(event)
    if current_path ~= nil then
        write_entry(current_path, current_duration, last_time_pos, event.reason)
    end
end)
