-- Alixer admission/fallback guard. Mixer intent and reconciliation remain in Rust.
-- Does not change global fallback or forbid explicit moves to existing targets.
local lutils = require ("linking-utils")
local cutils = require ("common-utils")
local state_dir = os.getenv("ALIXER_STATE_DIR") or os.getenv("LIXER_STATE_DIR") or
  ((os.getenv("XDG_STATE_HOME") or ((os.getenv("HOME") or "") .. "/.local/state")) .. "/alixer")
local function remembered_target(props)
  local file = io.open(state_dir .. "/guard.json", "r")
  if not file then return nil end
  local content = file:read("*a"); file:close()
  local ok, entries = pcall(function() return Json.Raw(content):parse() end)
  if not ok then return nil end
  local application = props["application.id"] or props["application.process.binary"] or props["application.name"]
  local name = props["media.name"] or props["node.description"] or props["node.name"]
  local target = nil
  for _, entry in ipairs(entries or {}) do
    if entry.application == application and entry.name == name then
      if target then return nil end -- ambiguous matches are not assigned
      target = entry.target
    end
  end
  return target
end
SimpleEventHook {
  name = "alixer/prevent-fallback",
  after = { "linking/find-defined-target" },
  before = { "linking/find-default-target", "linking/find-best-target" },
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "select-target" },
      Constraint { "media.class", "=", "Stream/Output/Audio" },
    },
  },
  execute = function(event)
    local _, _, si, props, _, target = lutils:unwrap_select_target_event(event)
    if target then return end
    local metadata = cutils.get_default_metadata_object()
    local requested = metadata and metadata:find(props["node.id"], "target.object")
    requested = requested or props["target.object"] or remembered_target(props)
    if requested and (tostring(requested):match("^alixer%.") or tostring(requested):match("^lixer%.")) then
      -- Stop before default selection, leaving the registered stream alive.
      event:stop_processing()
    end
  end
}:register()
