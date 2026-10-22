-- Alixer admission/fallback guard. Mixer intent and reconciliation remain in Rust.
-- Does not change global fallback or forbid explicit moves to existing targets.
local lutils = require ("linking-utils")
local cutils = require ("common-utils")
-- Client lifetime remains stable across DSP rebuilds and ends on both a
-- graceful stop and a crash. Never hold desktop playback after it disappears.
local engines = {}
local function engine_running()
  return next(engines) ~= nil
end
SimpleEventHook {
  name = "alixer/engine-connected",
  interests = { EventInterest { Constraint { "event.type", "=", "client-added" } } },
  execute = function(event)
    local client = event:get_subject()
    if client.properties["application.id"] == "org.alixer.Engine" then engines[client.id] = true end
  end,
}:register()

local function remembered_target(props)
  local metadata = cutils.get_default_metadata_object()
  local content = metadata and metadata:find(0, "alixer.saved-targets")
  if not content then return nil end
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
local function is_system_sound(props, si)
  -- Session-item properties do not necessarily retain custom event.id keys.
  local node = si:get_associated_proxy("node")
  local native = node and node.properties or {}
  local role = tostring(props["media.role"] or native["media.role"] or ""):lower()
  local event_id = props["event.id"] or native["event.id"]
  return role == "event" or role == "notification" or (event_id and event_id ~= "")
end

-- Resolve the original target with normal WirePlumber policy, then let Alixer
-- insert a per-event filter. Shared mute is applied before any sample reaches
-- the requested device, independently of client-side volume initialization.
SimpleEventHook {
  name = "alixer/system-sounds-target",
  after = "linking/find-best-target",
  before = "linking/prepare-link",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "select-target" },
      Constraint { "media.class", "=", "Stream/Output/Audio" },
    },
  },
  execute = function(event)
    local _, om, si, props, flags, target = lutils:unwrap_select_target_event(event)
    if not is_system_sound(props, si) then return end
    local engine = cutils.get_object_manager("node"):lookup {
      Constraint { "node.name", "#", "alixer.engine.*" },
    }
    if not engine then return end
    local node = si:get_associated_proxy("node")
    local pulse = node.properties["pulse.corked"] ~= nil
    if not pulse then
      if flags.peer_id then
        local old_link = lutils.lookupLink(si.id, flags.peer_id)
        if old_link then old_link:remove() end
        flags.peer_id = nil
      end
      local format, mode = si:get_ports_format()
      if mode ~= "dsp" then si:set_ports_format(format, "dsp", function() end) end
    end
    local metadata = cutils.get_default_metadata_object()
    if metadata and target and target.properties["node.name"] then
      metadata:set(tonumber(props["node.id"]), "alixer.system-sounds.target", "Spa:String", target.properties["node.name"])
    end
    local ingress = om:lookup {
      type = "SiLinkable",
      Constraint { "node.name", "=", "alixer.event." .. tostring(node.properties["object.serial"]) },
    }
    local ready = metadata and metadata:find(tonumber(props["node.id"]), "alixer.system-sounds.ready")
    if pulse and ingress and ready == ingress.properties["node.name"] then
      event:set_data("target", ingress)
    else
      event:stop_processing()
    end
  end,
}:register()

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
    if not engine_running() or is_system_sound(props, si) then return end
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

-- Retry only the waiting event once its processing path is connected.
SimpleEventHook {
  name = "alixer/system-sounds-ready",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "metadata-changed" },
      Constraint { "metadata.name", "=", "default" },
      Constraint { "event.subject.key", "=", "alixer.system-sounds.ready" },
      Constraint { "event.subject.value", "is-present" },
    },
  },
  execute = function(event)
    local props = event:get_properties()
    local si = cutils.get_object_manager("session-item"):lookup {
      type = "SiLinkable",
      Constraint { "node.id", "=", props["event.subject.id"] },
    }
    if si then event:get_source():call("push-event", "select-target", si, nil) end
  end,
}:register()

-- Release managed playback on disconnect, including old lingering endpoints
-- created by earlier versions. Leave the virtual microphone silent so an
-- attached recorder cannot unexpectedly fall back to a hardware microphone.
SimpleEventHook {
  name = "alixer/return-desktop-playback",
  interests = { EventInterest { Constraint { "event.type", "=", "client-removed" } } },
  execute = function(event)
    local id = event:get_subject().id
    if not engines[id] then return end
    engines[id] = nil
    if engine_running() then return end
    local nodes = cutils.get_object_manager("node")
    local metadata = cutils.get_default_metadata_object()
    local owned_ids, owned_serials = {}, {}
    local ingresses = {}
    for node in nodes:iterate() do
      local name = tostring(node.properties["node.name"] or "")
      if name:match("^alixer%.input%.") or name:match("^lixer%.input%.") or name:match("^alixer%.event%.") then
        owned_ids[tostring(node["bound-id"])] = true
        owned_serials[tostring(node.properties["object.serial"])] = true
        table.insert(ingresses, node)
      end
    end
    if metadata then
      metadata:set(0, "alixer.saved-targets", nil, nil)
      for node in nodes:iterate() do
        if node.properties["media.class"] == "Stream/Output/Audio" then
          local id = node["bound-id"]
          for _, key in ipairs({ "target.object", "target.node" }) do
            local target = metadata:find(id, key)
            local owned = key == "target.object" and owned_serials or owned_ids
            if target and (owned[tostring(target)] or tostring(target):match("^alixer%.input%.")
                or tostring(target):match("^lixer%.input%.") or tostring(target):match("^alixer%.event%.")) then
              metadata:set(id, key, nil, nil)
            end
          end
          metadata:set(id, "alixer.system-sounds.target", nil, nil)
          metadata:set(id, "alixer.system-sounds.ready", nil, nil)
        end
      end
    end
    for _, node in ipairs(ingresses) do node:request_destroy() end
    event:get_source():call("schedule-rescan", "linking")
  end,
}:register()
