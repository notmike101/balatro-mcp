-- AgentAutomation: file-backed automation bridge for Balatro.
-- Loaded by Lovely through lovely.toml by appending this file to main.lua.

CODA = CODA or {}
CODA.version = "0.6.0"
CODA.allowed_seed = "2K9H9HN"
CODA.command_path = "codex_command.lua"
CODA.observation_path = "codex_observation.json"
CODA.response_path = "codex_response.json"
CODA.poll_interval = 0.08
CODA.observe_interval = 0.25
CODA.poll_timer = 0
CODA.observe_timer = 0
CODA.session_id = CODA.session_id or (tostring(os.time()) .. "-" .. tostring(love.timer.getTime()))
CODA.observation_seq = CODA.observation_seq or 0
CODA.response_seq = CODA.response_seq or 0
CODA.last_command_id = CODA.last_command_id
CODA.last_command_source = CODA.last_command_source
CODA.play_history = {}
CODA.last_chips_total = nil
CODA.last_hands_played = nil
CODA._last_hand_num_captured = -1  -- highest hand_num already recorded
CODA.last_response = CODA.last_response or {
    ok = true,
    message = "AgentAutomation loaded",
    version = CODA.version
}

-- Helper: get current hand played counts from G.GAME.hands
local function _get_hand_played_counts()
    if not G or not G.GAME or not G.GAME.hands then return {} end
    local counts = {}
    for name, info in pairs(G.GAME.hands) do
        if info and type(info) == 'table' and info.played then
            counts[name] = info.played
        end
    end
    return counts
end

-- Fix up chips_earned from total_chips diffs. Call after any recording.
local function _recompute_earnings()
    local n = #CODA.play_history
    if n < 1 then return end
    CODA.play_history[1].chips_earned = CODA.play_history[1].total_chips
    for i = 2, n do
        CODA.play_history[i].chips_earned = 
            CODA.play_history[i].total_chips - CODA.play_history[i-1].total_chips
    end
end

-- Track per-hand scores and append to play_history
local function capture_hand_score()
    if not G or not G.GAME then return end

    local current_hands_played = G.GAME.hands_played or 0
    local chips_this = G.GAME.chips or 0

    -- Reset play_history when a new run starts (chips reset to 0, hands_played = 0)
    if chips_this == 0 and current_hands_played == 0 then
        CODA.play_history = {}
        CODA._chips_at_last_capture = 0
        CODA._last_hand_played_snapshot = _get_hand_played_counts()
        CODA._last_hand_num_captured = -1
        CODA.last_chips_total = chips_this
        return
    end

    -- Initialize baseline on FIRST call ever (regardless of state)
    if CODA.last_chips_total == nil then
        CODA.last_chips_total = chips_this
        CODA._chips_at_last_capture = 0  -- first hand scores from 0
        CODA._last_hand_played_snapshot = _get_hand_played_counts()
        return
    end

    -- Track score deltas during any round state (exclude menu/splash/gameover)
    local g_state = G.STATE
    if not G.STATES then return end
    local is_round_state = true
    for _, excluded in ipairs({G.STATES.SPLASH, G.STATES.MENU, G.STATES.GAMEOVER}) do
        if g_state == excluded then is_round_state = false; break; end
    end
    if not is_round_state then return end

    -- Record when chips increase since last capture baseline — chip delta triggers are reliable
    local score_delta = math.max(0, chips_this - CODA._chips_at_last_capture)

    if score_delta > 0 then
        -- Detect which hand type was just played by comparing played counts
        local current_snapshot = _get_hand_played_counts()
        local prev_snapshot = CODA._last_hand_played_snapshot or {}

        local changed_hand = nil
        for name, count in pairs(current_snapshot) do
            if count > (prev_snapshot[name] or 0) then
                changed_hand = name
                break
            end
        end

        -- Check if this hand_num already has an entry (dedup from multi-step scoring)
        local existing_entry = nil
        for _, entry in ipairs(CODA.play_history) do
            if entry.hand_num == current_hands_played then
                existing_entry = entry
                break
            end
        end

        if existing_entry then
            -- Update existing entry with latest chip values (multi-step scoring catching up)
            local new_delta = math.max(0, chips_this - CODA._chips_at_last_capture)
            existing_entry.chips_earned = math.max(existing_entry.chips_earned, new_delta)
            existing_entry.total_chips = chips_this
        elseif current_hands_played > (CODA._last_hand_num_captured or -1) then
-- DEBUG: removed noisy print statement --
            table.insert(CODA.play_history, {
                hand_num = current_hands_played,
                hand_type = changed_hand or 'Unknown',
                chips_earned = score_delta,
                total_chips = chips_this,
            })
        end

        -- Update baseline to latest chip value for next comparison
        CODA._chips_at_last_capture = chips_this
    end

    -- Recompute chips_earned from total_chips diffs (fixes multi-step scoring issues)
    _recompute_earnings()

    -- Update snapshot on every call (for next comparison)
    CODA._last_hand_played_snapshot = _get_hand_played_counts()
    CODA.last_hands_played = current_hands_played
end

local IMPORTANT_ABILITY_KEYS = {
    "name", "set", "extra", "bonus", "mult", "x_mult", "h_mult", "h_x_mult",
    "t_mult", "t_chips", "p_mult", "p_x_mult", "perma_bonus", "perma_mult",
    "perma_x_mult", "perma_h_chips", "perma_h_mult", "d_size", "hands",
    "discards", "consumeable", "max_highlighted", "min_highlighted"
}

local function json_escape(value)
    local replacements = {
        ['"'] = '\\"',
        ["\\"] = "\\\\",
        ["\b"] = "\\b",
        ["\f"] = "\\f",
        ["\n"] = "\\n",
        ["\r"] = "\\r",
        ["\t"] = "\\t"
    }
    return '"' .. tostring(value):gsub('[%z\1-\31\\"]', function(c)
        return replacements[c] or string.format("\\u%04x", string.byte(c))
    end) .. '"'
end

local function is_finite_number(value)
    return value == value and value ~= math.huge and value ~= -math.huge
end

local function is_array_table(value)
    local max_index = 0
    local count = 0
    for key, _ in pairs(value) do
        if type(key) ~= "number" or key < 1 or key ~= math.floor(key) then
            return false, 0
        end
        count = count + 1
        if key > max_index then max_index = key end
    end
    return count == max_index, max_index
end

local function encode_json(value, depth, seen)
    depth = depth or 0
    seen = seen or {}
    local value_type = type(value)

    if value_type == "nil" then
        return "null"
    elseif value_type == "boolean" then
        return value and "true" or "false"
    elseif value_type == "number" then
        return is_finite_number(value) and tostring(value) or "null"
    elseif value_type == "string" then
        return json_escape(value)
    elseif value_type ~= "table" then
        return json_escape("<" .. value_type .. ">")
    end

    if seen[value] or depth > 7 then
        return json_escape("<cycle>")
    end
    seen[value] = true

    local is_array, max_index = is_array_table(value)
    local parts = {}
    if is_array then
        for index = 1, max_index do
            parts[#parts + 1] = encode_json(value[index], depth + 1, seen)
        end
        seen[value] = nil
        return "[" .. table.concat(parts, ",") .. "]"
    end

    local keys = {}
    for key, _ in pairs(value) do
        keys[#keys + 1] = key
    end
    table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
    for _, key in ipairs(keys) do
        local key_type = type(key)
        if key_type == "string" or key_type == "number" or key_type == "boolean" then
            parts[#parts + 1] = json_escape(tostring(key)) .. ":" .. encode_json(value[key], depth + 1, seen)
        end
    end
    seen[value] = nil
    return "{" .. table.concat(parts, ",") .. "}"
end

local function state_name()
    if not G or not G.STATES then return nil end
    for name, value in pairs(G.STATES) do
        if value == G.STATE then return name end
    end
    return tostring(G.STATE)
end

local function stage_name()
    if not G or not G.STAGES then return nil end
    for name, value in pairs(G.STAGES) do
        if value == G.STAGE then return name end
    end
    return tostring(G.STAGE)
end

local function edition_name(edition)
    if not edition then return nil end
    if edition.negative then return "negative" end
    if edition.polychrome then return "polychrome" end
    if edition.holo then return "holographic" end
    if edition.foil then return "foil" end
    return edition.type
end

local function primitive_summary(value, depth, seen)
    depth = depth or 1
    seen = seen or {}
    local value_type = type(value)
    if value_type == "nil" or value_type == "boolean" or value_type == "number" or value_type == "string" then
        return value
    end
    if value_type ~= "table" or depth <= 0 or seen[value] then
        return nil
    end

    seen[value] = true
    local result = {}
    for key, child in pairs(value) do
        local key_type = type(key)
        local child_type = type(child)
        if key_type == "string" and (child_type == "nil" or child_type == "boolean" or child_type == "number" or child_type == "string") then
            result[key] = child
        elseif key_type == "string" and child_type == "table" and depth > 1 then
            result[key] = primitive_summary(child, depth - 1, seen)
        end
    end
    seen[value] = nil
    return result
end

local function raw_localization_summary(set, key)
    if not G or not G.localization or not G.localization.descriptions or not set or not key then
        return nil
    end
    local localization_set = set
    local group = G.localization.descriptions[localization_set]
    if not group and set == "Booster" then
        localization_set = "Other"
        group = G.localization.descriptions[localization_set]
    end
    local localization_key = key
    local entry = group and group[localization_key] or nil
    if not entry and set == "Booster" and type(key) == "string" then
        localization_key = string.gsub(key, "_%d+$", "")
        entry = group and group[localization_key] or nil
    end
    if not entry then return nil end

    return {
        set = localization_set,
        source_set = set ~= localization_set and set or nil,
        key = localization_key,
        source_key = key ~= localization_key and key or nil,
        name = entry.name,
        text = entry.text,
        unlock = entry.unlock
    }
end

local function ability_summary(ability)
    if not ability then return nil end
    local result = {}
    for _, key in ipairs(IMPORTANT_ABILITY_KEYS) do
        local value = ability[key]
        local value_type = type(value)
        if value_type == "nil" or value_type == "boolean" or value_type == "number" or value_type == "string" then
            result[key] = value
        elseif value_type == "table" then
            result[key] = primitive_summary(value, 1)
        end
    end
    return result
end

local function card_summary(card, index)
    if not card then return nil end
    local config = card.config or {}
    local center = config.center or {}
    local base = card.base or {}
    local ability = card.ability or {}
    local center_key = config.center_key or center.key
    local face_down = card.facing == "back"

    -- Mask identity fields when the card is face-down
    local display_name = nil
    if not face_down then
        display_name = ability.name or center.name or base.name
    end

    return {
        index = index,
        id = card.sort_id,
        name = display_name,
        set = (not face_down and ability.set) or center.set,
        center_key = center_key,
        effect = raw_localization_summary(center.set or ability.set, center_key),
        center_config = primitive_summary(center.config, 2),
        cost = card.cost,
        sell_cost = card.sell_cost,
        highlighted = not not card.highlighted,
        debuffed = not not card.debuff,
        facing = card.facing,
        edition = edition_name(card.edition),
        seal = (not face_down and card.seal) or nil,
        pinned = not not card.pinned,
        ability = ability_summary(ability),
        base = {
            name = (not face_down and base.name) or nil,
            suit = (not face_down and base.suit) or nil,
            value = (not face_down and base.value) or nil,
            id = (not face_down and base.id) or nil,
            nominal = (not face_down and base.nominal) or nil
        }
    }
end

local function area_cards(area)
    local cards = {}
    if area and area.cards then
        for index, card in ipairs(area.cards) do
            cards[#cards + 1] = card_summary(card, index)
        end
    end
    return cards
end

local function highlighted_indices(area)
    local indices = {}
    if area and area.cards and area.highlighted then
        for _, highlighted_card in ipairs(area.highlighted) do
            for index, card in ipairs(area.cards) do
                if card == highlighted_card then
                    indices[#indices + 1] = index
                    break
                end
            end
        end
    end
    return indices
end

local function area_state(area)
    if not area then return nil end
    return {
        count = area.cards and #area.cards or nil,
        limit = area.config and area.config.card_limit or nil,
        highlighted_limit = area.config and area.config.highlighted_limit or nil,
        highlighted = highlighted_indices(area)
    }
end

local function increment_count(table_value, key)
    if not key then return end
    table_value[key] = (table_value[key] or 0) + 1
end

local function card_collection_summary(area)
    if not area or not area.cards then return nil end
    local summary = {
        count = #area.cards,
        by_suit = {},
        by_rank = {},
        by_card_key = {},
        by_center_key = {},
        editions = {},
        seals = {}
    }
    for _, card in ipairs(area.cards) do
        -- Skip face-down cards to prevent identity leakage (e.g., The Fish boss)
        if card.facing == "back" then
            summary.count = summary.count - 1
            goto continue
        end
        local config = card.config or {}
        local base = card.base or {}
        increment_count(summary.by_suit, base.suit)
        increment_count(summary.by_rank, base.value or base.id)
        increment_count(summary.by_card_key, config.card_key)
        increment_count(summary.by_center_key, config.center_key)
        increment_count(summary.editions, edition_name(card.edition))
        increment_count(summary.seals, card.seal)
        ::continue::
    end
    return summary
end

local function center_summary(key, registry)
    local center = registry and registry[key] or nil
    return {
        key = key,
        name = center and center.name or nil,
        set = center and center.set or nil,
        effect = center and raw_localization_summary(center.set, key) or nil,
        rarity = center and center.rarity or nil,
        cost = center and center.cost or nil,
        config = center and primitive_summary(center.config, 2) or nil
    }
end

local function used_centers_summary(used_table, registry)
    local out = {}
    if used_table then
        for key, value in pairs(used_table) do
            if value then
                out[#out + 1] = center_summary(key, registry)
            end
        end
    end
    table.sort(out, function(a, b) return tostring(a.key) < tostring(b.key) end)
    return out
end

local function tags_summary()
    local out = {}
    if G and G.GAME and G.GAME.tags then
        for index, tag in ipairs(G.GAME.tags) do
            out[#out + 1] = {
                index = index,
                id = tag.ID,
                key = tag.key,
                name = tag.name,
                effect = raw_localization_summary("Tag", tag.key),
                triggered = not not tag.triggered,
                tally = tag.tally,
                ability = primitive_summary(tag.ability, 1),
                config = primitive_summary(tag.config, 2)
            }
        end
    end
    return out
end

local function collect_ui_nodes_from_node(node, box_name, out, seen, depth)
    if not node or seen[node] or (depth or 0) > 20 then return end
    seen[node] = true

    local config = node.config or {}
    if config.id or config.button or config.func or config.choice then
        out[#out + 1] = {
            box = box_name,
            id = config.id,
            button = config.button,
            func = config.func,
            choice = config.choice,
            chosen = config.chosen,
            visible = node.states and node.states.visible,
            click_can = node.states and node.states.click and node.states.click.can,
            collide_can = node.states and node.states.collide and node.states.collide.can,
            disabled = not not node.disable_button,
            x = node.T and node.T.x,
            y = node.T and node.T.y,
            w = node.T and node.T.w,
            h = node.T and node.T.h
        }
    end

    if node.children then
        for _, child in pairs(node.children) do
            collect_ui_nodes_from_node(child, box_name, out, seen, (depth or 0) + 1)
        end
    end
    if config.object and config.object.UIRoot then
        collect_ui_nodes_from_node(config.object.UIRoot, box_name .. ".object", out, seen, (depth or 0) + 1)
    end
end

local function collect_ui_nodes()
    local out = {}
    local seen = {}
    local boxes = {
        {name = "MAIN_MENU_UI", box = G and G.MAIN_MENU_UI},
        {name = "OVERLAY_MENU", box = G and G.OVERLAY_MENU},
        {name = "blind_select", box = G and G.blind_select},
        {name = "round_eval", box = G and G.round_eval},
        {name = "shop", box = G and G.shop},
        {name = "buttons", box = G and G.buttons}
    }

    if G and G.I and G.I.UIBOX then
        for index, box in ipairs(G.I.UIBOX) do
            boxes[#boxes + 1] = {name = "UIBOX_" .. tostring(index), box = box}
        end
    end

    for _, entry in ipairs(boxes) do
        if entry.box and entry.box.UIRoot then
            collect_ui_nodes_from_node(entry.box.UIRoot, entry.name, out, seen, 0)
        end
    end
    return out
end

local function find_ui_node_in_node(node, matcher, seen, depth)
    if not node or seen[node] or (depth or 0) > 20 then return nil end
    seen[node] = true
    if matcher(node) then return node end

    if node.children then
        for _, child in pairs(node.children) do
            local found = find_ui_node_in_node(child, matcher, seen, (depth or 0) + 1)
            if found then return found end
        end
    end
    if node.config and node.config.object and node.config.object.UIRoot then
        local found = find_ui_node_in_node(node.config.object.UIRoot, matcher, seen, (depth or 0) + 1)
        if found then return found end
    end
    return nil
end

local function find_ui_node(command)
    local target_id = command.ui_id or command.target_id
    local target_button = command.button
    local occurrence = tonumber(command.occurrence or command.index or 1) or 1
    local count = 0
    local matcher = function(node)
        local config = node.config or {}
        if target_id and tostring(config.id) ~= tostring(target_id) then return false end
        if target_button and tostring(config.button) ~= tostring(target_button) then return false end
        if not target_id and not target_button then return false end
        if node.disable_button then return false end
        if node.states and node.states.visible == false then return false end
        if node.states and node.states.click and node.states.click.can == false then return false end
        count = count + 1
        return count == occurrence
    end

    local boxes = {
        G and G.OVERLAY_MENU,
        G and G.MAIN_MENU_UI,
        G and G.blind_select,
        G and G.round_eval,
        G and G.shop,
        G and G.buttons
    }
    if G and G.I and G.I.UIBOX then
        for _, box in ipairs(G.I.UIBOX) do boxes[#boxes + 1] = box end
    end
    for _, box in ipairs(boxes) do
        if box and box.UIRoot then
            local seen = {}
            local found = find_ui_node_in_node(box.UIRoot, matcher, seen, 0)
            if found then return found end
        end
    end
    return nil
end

local poker_hands_source

local function blind_summary()
    if not G or not G.GAME or not G.GAME.blind then return nil end
    local blind = G.GAME.blind
    local config = blind.config and blind.config.blind or {}
    return {
        name = blind.name,
        key = config.key,
        effect = raw_localization_summary("Blind", config.key),
        chips = blind.chips,
        chip_text = blind.chip_text,
        dollars = blind.dollars,
        boss = not not blind.boss,
        disabled = not not blind.disabled,
        triggered = not not blind.triggered
    }
end

local function blind_choices_summary()
    if not G or not G.GAME or not G.GAME.round_resets then return nil end
    local resets = G.GAME.round_resets
    local choices = {}
    for _, blind_type in ipairs({"Small", "Big", "Boss"}) do
        local blind_key = resets.blind_choices and resets.blind_choices[blind_type]
        local blind = blind_key and G.P_BLINDS and G.P_BLINDS[blind_key] or nil
        local tag_key = resets.blind_tags and resets.blind_tags[blind_type] or nil
        local tag = tag_key and G.P_TAGS and G.P_TAGS[tag_key] or nil
        choices[blind_type] = {
            key = blind_key,
            name = blind and blind.name or nil,
            effect = blind_key and raw_localization_summary("Blind", blind_key) or nil,
            boss = blind and not not blind.boss or nil,
            mult = blind and blind.mult or nil,
            dollars = blind and blind.dollars or nil,
            state = resets.blind_states and resets.blind_states[blind_type] or nil,
            tag = tag_key,
            tag_name = tag and tag.name or nil,
            tag_effect = tag_key and raw_localization_summary("Tag", tag_key) or nil,
            tag_config = tag and primitive_summary(tag.config, 2) or nil
        }
    end
    return choices
end

local function round_summary()
    if not G or not G.GAME then return nil end
    local current = G.GAME.current_round or {}
    local resets = G.GAME.round_resets or {}
    return {
        seed = G.GAME.pseudorandom and G.GAME.pseudorandom.seed,
        dollars = G.GAME.dollars,
        bankrupt_at = G.GAME.bankrupt_at,
        chips = G.GAME.chips,
        chips_text = G.GAME.chips_text,
        ante = resets.ante,
        blind_ante = resets.blind_ante,
        boss_rerolled = resets.boss_rerolled,
        round = G.GAME.round,
        stake = G.GAME.stake,
        won = G.GAME.won,
        seeded = G.GAME.seeded,
        challenge = primitive_summary(G.GAME.challenge, 1),
        blind_on_deck = G.GAME.blind_on_deck,
        hands_left = current.hands_left,
        discards_left = current.discards_left,
        hands_played = current.hands_played,
        discards_used = current.discards_used,
        current_hand = current.current_hand and primitive_summary(current.current_hand, 2) or nil,
        current_voucher = current.voucher and center_summary(current.voucher, G.P_CENTERS) or nil,
        used_packs = current.used_packs,
        pack_choices = G.GAME.pack_choices,
        pack_size = G.GAME.pack_size,
        reroll_cost = current.reroll_cost,
        free_rerolls = current.free_rerolls,
        most_played_poker_hand = current.most_played_poker_hand,
        skips = G.GAME.skips,
        unused_discards = G.GAME.unused_discards,
        hands_played_total = G.GAME.hands_played,
        starting_params = primitive_summary(G.GAME.starting_params, 2),
        modifiers = primitive_summary(G.GAME.modifiers, 2),
        pool_flags = primitive_summary(G.GAME.pool_flags, 1),
        used_vouchers = used_centers_summary(G.GAME.used_vouchers, G.P_CENTERS),
        used_jokers = used_centers_summary(G.GAME.used_jokers, G.P_CENTERS),
        active_tags = tags_summary(),
        round_scores = primitive_summary(G.GAME.round_scores, 2),
        cards_played = primitive_summary(G.GAME.cards_played, 2),
        blind_states = resets.blind_states,
        blind_choices = blind_choices_summary(),
        hands = (function()
            local hands = poker_hands_source()
            return hands and primitive_summary(hands, 2) or nil
        end)()
    }
end

local function controller_locked()
    if not G or not G.CONTROLLER or not G.CONTROLLER.locks then return false end
    for _, value in pairs(G.CONTROLLER.locks) do
        if value then return true end
    end
    return false
end

local function event_queue_counts()
    if not G or not G.E_MANAGER or not G.E_MANAGER.queues then return nil end
    local counts = {}
    local total = 0
    for key, queue in pairs(G.E_MANAGER.queues) do
        counts[key] = #queue
        total = total + #queue
    end
    counts.total = total
    return counts
end

local function saved_game_file_summary()
    local profile = G and G.SETTINGS and G.SETTINGS.profile
    if not profile then return false, nil, nil end
    local path = tostring(profile) .. "/save.jkr"
    local info = love.filesystem.getInfo(path)
    if not info then return false, nil, nil end
    local cache_key = path .. ":" .. tostring(info.modtime or "") .. ":" .. tostring(info.size or "")
    if CODA.saved_game_cache and CODA.saved_game_cache.key == cache_key then
        return true, CODA.saved_game_cache.seed, CODA.saved_game_cache.hands
    end
    local seed = nil
    local hands = nil
    if get_compressed and STR_UNPACK then
        local ok, saved = pcall(function()
            local source = get_compressed(path)
            return source and STR_UNPACK(source) or nil
        end)
        if ok and saved and saved.GAME then
            seed = saved.GAME.pseudorandom and saved.GAME.pseudorandom.seed or nil
            hands = saved.GAME.hands
        end
    end
    CODA.saved_game_cache = {key = cache_key, seed = seed, hands = hands}
    return true, seed, hands
end

local POKER_HAND_KEYS = {
    "Flush Five",
    "Flush House",
    "Five of a Kind",
    "Straight Flush",
    "Four of a Kind",
    "Full House",
    "Flush",
    "Straight",
    "Three of a Kind",
    "Two Pair",
    "Pair",
    "High Card"
}

poker_hands_source = function()
    if G and G.STAGES and G.STAGE == G.STAGES.RUN and G.GAME and G.GAME.hands then
        local seed = G.GAME.pseudorandom and G.GAME.pseudorandom.seed or nil
        return G.GAME.hands, "live_run", true, seed
    end
    if G and G.SAVED_GAME and G.SAVED_GAME.GAME and G.SAVED_GAME.GAME.hands then
        local seed = G.SAVED_GAME.GAME.pseudorandom and G.SAVED_GAME.GAME.pseudorandom.seed or nil
        return G.SAVED_GAME.GAME.hands, "saved_run", true, seed
    end
    local _, saved_seed, saved_hands = saved_game_file_summary()
    if saved_hands then
        return saved_hands, "saved_run", true, saved_seed
    end
    local defaults = G and G.GAME and G.GAME.hands or nil
    return defaults, "menu_defaults", false, nil
end

local function poker_hands_summary()
    local hands, source, valid_for_scoring, source_seed = poker_hands_source()
    local values = {}
    for _, key in ipairs(POKER_HAND_KEYS) do
        local hand = hands and hands[key] or nil
        if hand then
            values[key] = {
                key = key,
                display_name = localize and localize(key, "poker_hands") or key,
                order = hand.order,
                visible = not not hand.visible,
                level = hand.level,
                chips = hand.chips,
                mult = hand.mult,
                base_chips = hand.s_chips,
                base_mult = hand.s_mult,
                chips_per_level = hand.l_chips,
                mult_per_level = hand.l_mult,
                played = hand.played,
                played_this_round = hand.played_this_round,
                played_this_ante = hand.played_this_ante
            }
        end
    end
    return {
        schema = "balatro-poker-hand-values/v1",
        source = source,
        source_seed = source_seed,
        valid_for_scoring = valid_for_scoring,
        values = values
    }
end

local function readiness_summary()
    if not G then return nil end
    local save_file_present, save_file_seed = saved_game_file_summary()
    return {
        state_complete = not not G.STATE_COMPLETE,
        controller_locked = controller_locked(),
        main_menu_ui_present = not not G.MAIN_MENU_UI,
        overlay_menu_present = not not G.OVERLAY_MENU,
        overlay_tutorial_present = not not G.OVERLAY_TUTORIAL,
        tutorial_complete = G.SETTINGS and (G.SETTINGS.tutorial_complete == true),
        tutorial_progress_present = G.SETTINGS and (G.SETTINGS.tutorial_progress ~= nil),
        current_setup = G.SETTINGS and G.SETTINGS.current_setup,
        profile = G.SETTINGS and G.SETTINGS.profile,
        saved_game_present = save_file_present or G.SAVED_GAME ~= nil,
        saved_game_loaded = G.SAVED_GAME ~= nil,
        saved_game_seed = G.SAVED_GAME and G.SAVED_GAME.GAME and G.SAVED_GAME.GAME.pseudorandom and G.SAVED_GAME.GAME.pseudorandom.seed or save_file_seed,
        ui_boxes = G.I and G.I.UIBOX and #G.I.UIBOX or nil,
        event_queues = event_queue_counts(),
        timers = G.TIMERS and {REAL = G.TIMERS.REAL, TOTAL = G.TIMERS.TOTAL, UPTIME = G.TIMERS.UPTIME} or nil,
        blind_select_ready = not not G.blind_select,
        round_eval_ready = not not G.round_eval,
        shop_ready = not not G.shop,
        hand_ready = not not (G.hand and G.hand.cards and #G.hand.cards > 0),
        pack_ready = not not (G.pack_cards and G.pack_cards.cards and #G.pack_cards.cards > 0),
        event_queue_empty = G.E_MANAGER and G.E_MANAGER.queues and not next(G.E_MANAGER.queues) or nil
    }
end

local function run_info_summary()
    if not G or not G.GAME then return nil end
    local g = G.GAME
    return {
        ante = g.ante,
        round = g.round,
        blind_on_deck = g.blind_on_deck,
        target_chips = g.blind and g.blind.chip_text or nil,
        current_score = g.current_round_scores and primitive_summary(g.current_round_scores, 2) or nil,
        total_score = g.total_score or nil,
        round_scores = primitive_summary(g.round_scores, 2),
        cards_played = primitive_summary(g.cards_played, 2),
        hand_upgrades = (function()
            local hands = poker_hands_source()
            return hands and primitive_summary(hands, 2) or nil
        end)(),
        jokers = used_centers_summary(g.used_jokers, G.P_CENTERS),
        vouchers = used_centers_summary(g.used_vouchers, G.P_CENTERS),
        dollars = g.dollars or nil,
        chips_total = g.chips and primitive_summary(g.chips, 2) or nil,
    }
end

local function available_actions()
    local actions = {"observe", "click", "speed", "skip_tutorial", "setup_new_run"}
    if not G or not G.STATES then return actions end

    if G.STATE == G.STATES.SPLASH or G.STATE == G.STATES.MENU then
        actions[#actions + 1] = "start_run"
    end
    if G.STATE == G.STATES.BLIND_SELECT then
        actions[#actions + 1] = "select_blind"
        actions[#actions + 1] = "skip_blind"
        actions[#actions + 1] = "reroll_boss"
    end
    if G.STATE == G.STATES.SELECTING_HAND then
        actions[#actions + 1] = "play"
        actions[#actions + 1] = "discard"
        actions[#actions + 1] = "sort_hand"
        actions[#actions + 1] = "move_card"
        actions[#actions + 1] = "use"
        actions[#actions + 1] = "preview_hand"
        actions[#actions + 1] = "sell"
    end
    if G.STATE == G.STATES.ROUND_EVAL then
        actions[#actions + 1] = "cash_out"
    end
    if G.STATE == G.STATES.SHOP then
        actions[#actions + 1] = "buy"
        actions[#actions + 1] = "buy_and_use"
        actions[#actions + 1] = "reroll_shop"
        actions[#actions + 1] = "next_round"
        actions[#actions + 1] = "move_card"
        actions[#actions + 1] = "use"
        actions[#actions + 1] = "sell"
    end
    if G.STATE == G.STATES.TAROT_PACK or G.STATE == G.STATES.PLANET_PACK or
       G.STATE == G.STATES.SPECTRAL_PACK or G.STATE == G.STATES.STANDARD_PACK or
       G.STATE == G.STATES.BUFFOON_PACK then
        actions[#actions + 1] = "use"
        actions[#actions + 1] = "skip_booster"
    end
    return actions
end

function CODA.observe()
    -- Capture scores during observe cycle (0.25s after play command, E_MANAGER should be done)
    capture_hand_score()

    CODA.observation_seq = CODA.observation_seq + 1
    local observation = {
        bridge = {
            loaded = true,
            version = CODA.version,
            session_id = CODA.session_id,
            observation_seq = CODA.observation_seq,
            observed_at_ms = os.time() * 1000 + math.floor((love.timer.getTime() % 1) * 1000),
            response_seq = CODA.response_seq,
            save_dir = love.filesystem.getSaveDirectory(),
            last_command_id = CODA.last_command_id,
            last_response = CODA.last_response
        },
        game = {
            state = G and G.STATE,
            state_name = state_name(),
            stage = G and G.STAGE,
            stage_name = stage_name(),
            speed = G and G.SETTINGS and G.SETTINGS.GAMESPEED,
            fps_cap = G and G.FPS_CAP
        },
        ready = readiness_summary(),
        run_info = run_info_summary(),
        round = round_summary(),
        poker_hands = poker_hands_summary(),
        deprecated_fields = {"round.hands", "run_info.hand_upgrades"},
        blind = blind_summary(),
        play_history = CODA.play_history or {},
        ui = collect_ui_nodes(),
        areas = {
            hand = area_cards(G and G.hand),
            hand_highlighted = highlighted_indices(G and G.hand),
            jokers = area_cards(G and G.jokers),
            consumeables = area_cards(G and G.consumeables),
            shop_jokers = area_cards(G and G.shop_jokers),
            shop_vouchers = area_cards(G and G.shop_vouchers),
            shop_booster = area_cards(G and G.shop_booster),
            pack_cards = area_cards(G and G.pack_cards),
            play = area_cards(G and G.play),
            deck = area_cards(G and G.deck),
            discard = area_cards(G and G.discard),
            deck_count = G and G.deck and G.deck.cards and #G.deck.cards or nil,
            discard_count = G and G.discard and G.discard.cards and #G.discard.cards or nil,
            deck_summary = card_collection_summary(G and G.deck),
            discard_summary = card_collection_summary(G and G.discard),
            state = {
                hand = area_state(G and G.hand),
                jokers = area_state(G and G.jokers),
                consumeables = area_state(G and G.consumeables),
                shop_jokers = area_state(G and G.shop_jokers),
                shop_vouchers = area_state(G and G.shop_vouchers),
                shop_booster = area_state(G and G.shop_booster),
                pack_cards = area_state(G and G.pack_cards),
                deck = area_state(G and G.deck),
                discard = area_state(G and G.discard)
            }
        },
        available_actions = available_actions()
    }
    return observation
end

local function write_json_file(path, value)
    love.filesystem.write(path, encode_json(value))
end

function CODA.write_observation()
    local ok, observation = pcall(CODA.observe)
    if ok then
        write_json_file(CODA.observation_path, observation)
    else
        write_json_file(CODA.observation_path, {
            bridge = {
                loaded = true,
                version = CODA.version,
                session_id = CODA.session_id,
                observation_seq = CODA.observation_seq,
                response_seq = CODA.response_seq
            },
            error = tostring(observation)
        })
    end
end

local function write_response(response)
    CODA.response_seq = CODA.response_seq + 1
    response.response_seq = CODA.response_seq
    response.session_id = CODA.session_id
    response.observation_seq = CODA.observation_seq
    CODA.last_response = response
    write_json_file(CODA.response_path, response)
end

local function area_by_name(name)
    if not G then return nil end
    local areas = {
        hand = G.hand,
        jokers = G.jokers,
        consumeables = G.consumeables,
        consumables = G.consumeables,
        shop_jokers = G.shop_jokers,
        shop_vouchers = G.shop_vouchers,
        shop_booster = G.shop_booster,
        pack_cards = G.pack_cards,
        play = G.play,
        deck = G.deck,
        discard = G.discard
    }
    return areas[name]
end

local function card_from_command(command, default_area)
    local area_name = command.area or default_area
    local index = tonumber(command.index or command.card or command.target_card)
    if not area_name then return nil, nil, "missing area" end
    if not index then return nil, nil, "missing card index" end
    local area = area_by_name(area_name)
    if not area or not area.cards then return nil, nil, "area not available: " .. tostring(area_name) end
    local card = area.cards[index]
    if not card then return nil, nil, "card index not available: " .. tostring(index) end
    local expected_id = command.card_id or command.instance_id
    if expected_id and tostring(card.sort_id) ~= tostring(expected_id) then
        return nil, nil, "card id mismatch at index " .. tostring(index) .. ": expected " .. tostring(expected_id) .. ", found " .. tostring(card.sort_id)
    end
    return card, area, nil
end

local function select_hand_cards(indices, expected_ids)
    if not G or not G.hand or not G.hand.cards then return false, "hand not available" end
    if type(indices) ~= "table" then return false, "cards must be a table of 1-based hand indices" end
    if expected_ids and #expected_ids ~= #indices then return false, "card id count must match hand card count" end
    G.hand:unhighlight_all()
    for position, raw_index in ipairs(indices) do
        local index = tonumber(raw_index)
        local card = index and G.hand.cards[index] or nil
        if not card then return false, "hand card index not available: " .. tostring(raw_index) end
        local expected_id = expected_ids and expected_ids[position] or nil
        if expected_id and tostring(card.sort_id) ~= tostring(expected_id) then
            return false, "hand card id mismatch at index " .. tostring(index) .. ": expected " .. tostring(expected_id) .. ", found " .. tostring(card.sort_id)
        end
        G.hand:add_to_highlighted(card, true)
    end
    return true, nil
end

local function command_start_run(command)
    if not G or not G.FUNCS or not G.FUNCS.start_run then return false, "start_run unavailable" end
    local requested_seed = command.seed and tostring(command.seed) or ""
    local is_resume = G.SAVED_GAME ~= nil
    if is_resume and requested_seed ~= "" then
        local saved_seed = G.SAVED_GAME.GAME and G.SAVED_GAME.GAME.pseudorandom and G.SAVED_GAME.GAME.pseudorandom.seed or nil
        if tostring(saved_seed or "") == CODA.allowed_seed then
            return false, "saved run exists; resume without supplying a seed"
        end
        if requested_seed ~= CODA.allowed_seed or not G.SETTINGS or G.SETTINGS.current_setup ~= "New Run" then
            return false, "wrong-seed save recovery requires New Run setup and allowed seed"
        end
        G.SAVED_GAME = nil
        is_resume = false
    end
    if not is_resume and requested_seed ~= CODA.allowed_seed then
        return false, "seed rejected: only " .. CODA.allowed_seed .. " is allowed"
    end
    local args = {}
    if command.stake then args.stake = tonumber(command.stake) end
    if not is_resume then args.seed = requested_seed end
    G.FUNCS.start_run(nil, args)
    return true, is_resume and "saved run resume queued" or "seeded run start queued"
end

local function command_setup_run()
    if not G or not G.FUNCS or not G.FUNCS.setup_run then return false, "setup_run unavailable" end
    G.FUNCS.setup_run({config = {id = "main_menu_play"}})
    return true, "queued setup_run"
end

local function command_start_setup_run()
    if not G or not G.FUNCS or not G.FUNCS.start_setup_run then return false, "start_setup_run unavailable" end
    G.FUNCS.start_setup_run({config = {button = "start_setup_run"}})
    return true, "queued start_setup_run"
end

local function safe_remove(object)
    if object and object.remove then
        pcall(function() object:remove() end)
    end
end

local function command_setup_new_run()
    if not G or not G.SETTINGS then return false, "settings unavailable" end
    if G.SAVED_GAME then
        local saved_seed = G.SAVED_GAME.GAME and G.SAVED_GAME.GAME.pseudorandom and G.SAVED_GAME.GAME.pseudorandom.seed or nil
        if tostring(saved_seed or "") == CODA.allowed_seed then
            return false, "saved run exists; resume it instead of starting a new run"
        end
        G.SAVED_GAME = nil
    end
    G.SETTINGS.current_setup = "New Run"

    if not G.OVERLAY_MENU then
        if not G.FUNCS or not G.FUNCS.setup_run then return true, "selected New Run" end
        G.FUNCS.setup_run({config = {id = "main_menu_play"}})
    end

    local tab_button = G.OVERLAY_MENU and G.OVERLAY_MENU.get_UIE_by_ID and G.OVERLAY_MENU:get_UIE_by_ID("tab_but_New Run") or nil
    if tab_button and G.FUNCS and G.FUNCS.change_tab then
        G.FUNCS.change_tab(tab_button)
        return true, "selected New Run tab"
    end

    local tab_contents = G.OVERLAY_MENU and G.OVERLAY_MENU.get_UIE_by_ID and G.OVERLAY_MENU:get_UIE_by_ID("tab_contents") or nil
    if tab_contents and tab_contents.config and G.UIDEF and G.UIDEF.run_setup_option and UIBox then
        safe_remove(tab_contents.config.object)
        tab_contents.config.object = UIBox{
            definition = G.UIDEF.run_setup_option("New Run"),
            config = {offset = {x = 0, y = 0}, parent = tab_contents, type = "cm"}
        }
        if tab_contents.UIBox and tab_contents.UIBox.recalculate then
            tab_contents.UIBox:recalculate()
        end
        return true, "rebuilt New Run tab"
    end

    return true, "selected New Run"
end

local function command_skip_tutorial()
    if not G or not G.SETTINGS then return false, "settings unavailable" end

    G.F_SKIP_TUTORIAL = true
    G.SETTINGS.tutorial_complete = true
    G.SETTINGS.tutorial_progress = nil

    if G.OVERLAY_TUTORIAL then
        safe_remove(G.OVERLAY_TUTORIAL.Jimbo)
        safe_remove(G.OVERLAY_TUTORIAL.content)
        safe_remove(G.OVERLAY_TUTORIAL)
        G.OVERLAY_TUTORIAL = nil
    end

    if G.save_settings then pcall(function() G:save_settings() end) end
    if G.save_progress then pcall(function() G:save_progress() end) end

    local refreshed_menu = false
    if G.STATES and G.STATE == G.STATES.MENU and set_main_menu_UI then
        safe_remove(G.MAIN_MENU_UI)
        G.MAIN_MENU_UI = nil
        set_main_menu_UI()
        refreshed_menu = true
    end

    return true, refreshed_menu and "tutorial skipped; refreshed main menu" or "tutorial skipped"
end

-- Shared: resolve which blind type to act on (Small/Big/Boss)
local function resolve_blind_type(command)
    if command and command.blind_type then
        local bt = tostring(command.blind_type)
        if bt == "small" or bt == "Small" then return "Small" end
        if bt == "big" or bt == "Big" then return "Big" end
        if bt == "boss" or bt == "Boss" then return "Boss" end
    end
    return (G and G.GAME and G.GAME.blind_on_deck) or "Small"
end

local function command_select_blind(command)
    if not G or not G.GAME then
        return false, "game state unavailable"
    end
    local blind_type = resolve_blind_type(command)
    -- Validate the blind type exists in choices
    local valid_types = {"Small", "Big", "Boss"}
    if not G.GAME.round_resets then return false, "round_resets unavailable" end
    local blind_key = G.GAME.round_resets.blind_choices and G.GAME.round_resets.blind_choices[blind_type]
    if not blind_key then return false, "blind key unavailable for " .. tostring(blind_type) end

    -- Set up the blind selection manually without calling G.FUNCS.select_blind()
    -- which adds events that can crash when processed out of normal game flow
    G.GAME.facing_blind = true
    G.GAME.round_resets.blind = G.P_BLINDS and G.P_BLINDS[blind_key] or nil
    G.GAME.round_resets.blind_states[blind_type] = "Current"

    -- Remove blind select UI if it exists to prevent stale references
    if G.blind_select then
        safe_remove(G.blind_select)
        G.blind_select = nil
    end
    if G.blind_prompt_box then
        safe_remove(G.blind_prompt_box)
        G.blind_prompt_box = nil
    end

    -- Advance to the round directly using E_MANAGER events
    -- This mirrors what select_blind does but in a controlled way
    if G.E_MANAGER and Event then
        -- Preserve current ante; new_round() increments it for the next round
        inc_career_stat('c_rounds', 1)
        -- Add immediate event to start the round after current frame completes
        G.E_MANAGER:add_event(Event({
            trigger = 'after',
            delay = 0,
            blocking = false,
            blockable = false,
            func = function()
                new_round()
                return true
            end
        }))
    else
        -- Fallback: just set state and hope the game loop picks it up
        G.GAME.blind_on_deck = blind_type
    end

    return true, "queued select_blind " .. tostring(blind_type)
end

local function command_skip_blind(command)
    if not G or not G.GAME or not G.GAME.round_resets then return false, "blind state unavailable" end
    if not G.blind_select then return false, "blind select UI is not ready" end

    local skipped = resolve_blind_type(command)
    G.GAME.blind_on_deck = skipped
    local skip_to = skipped == "Small" and "Big" or skipped == "Big" and "Boss" or "Boss"
    local tag_key = G.GAME.round_resets and G.GAME.round_resets.blind_tags and G.GAME.round_resets.blind_tags[skipped]
    if tag_key and Tag and add_tag then
        add_tag(Tag(tag_key, nil, skipped))
    end

    G.GAME.skips = (G.GAME.skips or 0) + 1
    G.GAME.round_resets.blind_states[skipped] = "Skipped"
    G.GAME.round_resets.blind_states[skip_to] = "Select"
    G.GAME.blind_on_deck = skip_to

    if G.jokers and G.jokers.cards then
        for i = 1, #G.jokers.cards do
            G.jokers.cards[i]:calculate_joker({skip_blind = true})
        end
    end
    if G.GAME.tags then
        for i = 1, #G.GAME.tags do
            G.GAME.tags[i]:apply_to_run({type = "immediate"})
        end
        for i = 1, #G.GAME.tags do
            if G.GAME.tags[i]:apply_to_run({type = "new_blind_choice"}) then break end
        end
    end
    if save_run then save_run() end
    return true, "queued skip_blind " .. tostring(skipped)
end

local function command_play(command)
    if not G or not G.FUNCS or not G.FUNCS.play_cards_from_highlighted then return false, "play callback unavailable" end
    local ok, err = select_hand_cards(command.cards, command.card_ids or command.instance_ids)
    if not ok then return false, err end
    -- Call the play function and ensure it processes by adding a follow-up event
    G.FUNCS.play_cards_from_highlighted({})
    -- Force event processing to complete in next frame
    if G.E_MANAGER then
        G.E_MANAGER:add_event(Event({trigger = 'after', delay = 0, blocking = false, func = function() return true end}))
    end
    return true, "queued play"
end

local function command_discard(command)
    if not G or not G.FUNCS or not G.FUNCS.discard_cards_from_highlighted then return false, "discard callback unavailable" end
    local ok, err = select_hand_cards(command.cards, command.card_ids or command.instance_ids)
    if not ok then return false, err end
    G.FUNCS.discard_cards_from_highlighted({})
    if G.E_MANAGER then
        G.E_MANAGER:add_event(Event({trigger = 'after', delay = 0, blocking = false, func = function() return true end}))
    end
    return true, "queued discard"
end

local function command_buy(command, buy_and_use)
    if not G or not G.FUNCS or not G.FUNCS.buy_from_shop then return false, "buy callback unavailable" end
    local card, _, err = card_from_command(command, command.area or "shop_jokers")
    if err then return false, err end

    if card.ability and (card.ability.set == "Voucher" or card.ability.set == "Booster") then
        if not G.FUNCS.use_card then return false, "use_card callback unavailable" end
        G.FUNCS.use_card({config = {ref_table = card, button = "use_card"}})
        return true, "queued " .. tostring(card.ability.set)
    end

    G.FUNCS.buy_from_shop({config = {ref_table = card, id = buy_and_use and "buy_and_use" or nil}})
    return true, "queued " .. (buy_and_use and "buy_and_use" or "buy")
end

local function command_use(command)
    if not G or not G.FUNCS or not G.FUNCS.use_card then return false, "use callback unavailable" end
    if command.targets then
        local ok, err = select_hand_cards(command.targets, command.target_card_ids or command.target_instance_ids)
        if not ok then return false, err end
    end
    local card, _, err = card_from_command(command, "consumeables")
    if err then return false, err end
    G.FUNCS.use_card({config = {ref_table = card, button = "use_card"}})
    return true, "queued use"
end

local function command_sell(command)
    if not G or not G.FUNCS or not G.FUNCS.sell_card then return false, "sell callback unavailable" end
    local card, _, err = card_from_command(command, command.area or "jokers")
    if err then return false, err end
    G.FUNCS.sell_card({config = {ref_table = card, button = "sell_card"}})
    return true, "queued sell"
end

local function command_click(command)
    if not G or not G.CONTROLLER then return false, "controller unavailable" end
    local x = tonumber(command.x)
    local y = tonumber(command.y)
    if not x or not y then return false, "click requires x and y screen coordinates" end
    G.CONTROLLER:set_HID_flags("mouse")
    G.CONTROLLER:queue_L_cursor_press(x, y)
    if G.E_MANAGER and Event then
        G.E_MANAGER:add_event(Event({
            trigger = "after",
            delay = 0.03,
            blocking = false,
            blockable = false,
            func = function()
                G.CONTROLLER:L_cursor_release(x, y)
                return true
            end
        }))
    else
        G.CONTROLLER:L_cursor_release(x, y)
    end
    return true, "queued click"
end

local function command_ui_click(command)
    local node = find_ui_node(command)
    if not node then
        local target_id = command.ui_id or command.target_id
        local target_button = command.button
        if target_button == "select_blind" or target_id == "select_blind_button" then
            local ok, message = command_select_blind()
            return ok, ok and ("ui node not found; " .. message) or message
        end
        if target_button == "cash_out" or target_id == "cash_out_button" then
            if G and G.FUNCS and G.FUNCS.cash_out then
                G.FUNCS.cash_out({config = {button = "cash_out"}})
                return true, "ui node not found; queued cash_out"
            end
            return false, "cash_out callback unavailable"
        end
        if target_button == "toggle_shop" or target_id == "next_round_button" then
            if G and G.FUNCS and G.FUNCS.toggle_shop then
                G.FUNCS.toggle_shop({config = {button = "toggle_shop"}})
                return true, "ui node not found; queued toggle_shop"
            end
            return false, "toggle_shop callback unavailable"
        end
        if target_button == "start_setup_run" then
            local ok, message = command_start_setup_run()
            return ok, ok and ("ui node not found; " .. message) or message
        end
        if target_button == "setup_run" or (target_id == "main_menu_play" and G and G.SETTINGS and G.SETTINGS.tutorial_complete) then
            local ok, message = command_setup_run()
            return ok, ok and ("ui node not found; " .. message) or message
        end
        if target_button == "start_run" or target_id == "main_menu_play" then
            local ok, message = command_start_run(command)
            return ok, ok and ("ui node not found; " .. message) or message
        end
        return false, "ui node not found"
    end
    if not node.click then
        return false, "ui node is not clickable"
    end
    if node.disable_button then
        return false, "ui node is disabled"
    end
    local node_button = node.config and node.config.button or node.button
    if node_button == "start_run" then
        if not G.SAVED_GAME then
            return false, "new run UI start blocked; use guarded start_run with seed " .. CODA.allowed_seed
        end
        local saved_seed = G.SAVED_GAME.GAME and G.SAVED_GAME.GAME.pseudorandom and G.SAVED_GAME.GAME.pseudorandom.seed or nil
        if saved_seed and tostring(saved_seed) ~= CODA.allowed_seed then
            return false, "saved run seed rejected: " .. tostring(saved_seed)
        end
    end
    node:click()
    return true, "clicked ui node"
end

local function command_ensure_menu_ui()
    if not G or not G.STATES or G.STATE ~= G.STATES.MENU then
        return false, "not in menu"
    end
    if G.MAIN_MENU_UI then
        return true, "main menu UI already present"
    end
    if not set_main_menu_UI then
        return false, "set_main_menu_UI unavailable"
    end
    set_main_menu_UI()
    return true, "created main menu UI"
end

local function command_speed(command)
    if G and G.SETTINGS and command.game_speed then
        G.SETTINGS.GAMESPEED = tonumber(command.game_speed) or G.SETTINGS.GAMESPEED
    end
    if G and command.fps_cap then
        G.FPS_CAP = tonumber(command.fps_cap) or G.FPS_CAP
    end
    return true, "updated speed"
end

local function command_sort_hand(command)
    if not G or not G.FUNCS then return false, "sort callbacks unavailable" end
    if command.mode == "suit" and G.FUNCS.sort_hand_suit then
        G.FUNCS.sort_hand_suit({})
        return true, "sorted hand by suit"
    elseif G.FUNCS.sort_hand_value then
        G.FUNCS.sort_hand_value({})
        return true, "sorted hand by value"
    end
    return false, "sort callback unavailable"
end

local function command_move_card(command)
    local area_name = command.area or "jokers"
    local from_index = tonumber(command.from_index or command.from or command.index)
    local to_index = tonumber(command.to_index or command.to)
    if not from_index or not to_index then return false, "move_card requires from_index and to_index" end

    local area = area_by_name(area_name)
    if not area or not area.cards then return false, "area not available: " .. tostring(area_name) end
    if from_index < 1 or from_index > #area.cards then return false, "from_index out of range" end
    if to_index < 1 or to_index > #area.cards then return false, "to_index out of range" end
    if from_index == to_index then return true, "move_card no-op" end

    local card = area.cards[from_index]
    local expected_id = command.card_id or command.instance_id
    if expected_id and tostring(card.sort_id) ~= tostring(expected_id) then
        return false, "card id mismatch at from_index"
    end

    table.remove(area.cards, from_index)
    table.insert(area.cards, to_index, card)
    for index, area_card in ipairs(area.cards) do
        area_card.rank = index
    end
    if area.align_cards then area:align_cards() end
    return true, "moved card in " .. tostring(area_name)
end

local function command_cash_out()
    if not G or not G.FUNCS or not G.FUNCS.cash_out then return false, "cash_out callback unavailable" end
    if not G.round_eval then return false, "round evaluation UI is not ready" end
    G.FUNCS.cash_out({config = {button = "cash_out"}})
    return true, "queued cash_out"
end

local function command_next_round()
    if not G or not G.FUNCS or not G.FUNCS.toggle_shop then return false, "next_round callback unavailable" end
    if not G.shop then return false, "shop UI is not ready" end
    G.FUNCS.toggle_shop({})
    return true, "queued next_round"
end

local function command_reroll_boss()
    if not G or not G.GAME or not G.FUNCS or not G.FUNCS.reroll_boss then
        return false, "reroll_boss callback unavailable"
    end
    if not G.blind_select or not G.blind_select_opts or not G.blind_select_opts.boss then
        return false, "blind select boss UI is not ready"
    end

    local dollars = tonumber(G.GAME.dollars) or 0
    local bankrupt_at = tonumber(G.GAME.bankrupt_at) or 0
    local used_vouchers = G.GAME.used_vouchers or {}
    local round_resets = G.GAME.round_resets or {}
    local can_pay = dollars - bankrupt_at >= 10
    local can_reroll = used_vouchers["v_retcon"] or (used_vouchers["v_directors_cut"] and not round_resets.boss_rerolled)
    if not can_pay then return false, "not enough dollars to reroll boss" end
    if not can_reroll then return false, "boss reroll voucher unavailable" end

    G.FUNCS.reroll_boss({})
    return true, "queued reroll_boss"
end

local function command_preview_hand(command)
    if not G or not G.hand then return false, 'hand unavailable' end
    local indices = command.cards
    if type(indices) ~= 'table' or #indices == 0 then return false, 'cards required' end

    -- Resolve expected card IDs (support single or list forms)
    local expected_ids = nil
    if command.card_ids or command.instance_ids then
        expected_ids = command.card_ids or command.instance_ids
    elseif command.card_id or command.instance_id then
        expected_ids = { command.card_id or command.instance_id }
    end

    G.hand:unhighlight_all()
    for position, raw_index in ipairs(indices) do
        local index = tonumber(raw_index)
        local card = G.hand.cards[index]
        if not card then return false, "preview card index not available: " .. tostring(raw_index) end

        -- Validate card ID if provided
        if expected_ids then
            local expected_id = expected_ids[position] or expected_ids[1]
            if expected_id and tostring(card.sort_id) ~= tostring(expected_id) then
                return false, "preview card id mismatch at index " .. tostring(index) .. ": expected " .. tostring(expected_id) .. " got " .. tostring(card.sort_id)
            end
        end

        G.hand:add_to_highlighted(card, true)
    end
    if G.E_MANAGER then
        G.E_MANAGER:add_event(Event({trigger = 'after', delay = 0.05, blocking = false, func = function() return true end}))
    end
    local ch = G.GAME and G.GAME.current_round and G.GAME.current_round.current_hand
    local result = {
        handname = ch and (ch.handname_text or ch.handname or ''),
        chip_total = ch and (ch.chip_total or 0),
        mult = ch and (ch.mult or 0),
        chips = ch and (ch.chips or 0),
    }
    G.hand:unhighlight_all()
    return true, 'preview done'
end

local function command_move_joker(command)
    local from_index = tonumber(command.from_index or command.from or command.index)
    local to_index = tonumber(command.to_index or command.to)
    if not from_index or not to_index then return false, "move_joker requires from_index and to_index" end

    if not G or not G.jokers or not G.jokers.cards then return false, "jokers area unavailable" end
    local cards = G.jokers.cards
    if from_index < 1 or from_index > #cards then return false, "from_index out of range (1.." .. #cards .. ")" end
    if to_index < 1 or to_index > #cards then return false, "to_index out of range (1.." .. #cards .. ")" end
    if from_index == to_index then return true, "move_joker no-op" end

    local card = cards[from_index]
    local expected_id = command.card_id or command.instance_id
    if expected_id and tostring(card.sort_id) ~= tostring(expected_id) then
        return false, "joker id mismatch at from_index: expected " .. tostring(expected_id) .. " got " .. tostring(card.sort_id)
    end

    table.remove(cards, from_index)
    table.insert(cards, to_index, card)
    return true, "moved joker from slot " .. from_index .. " to slot " .. to_index
end

function CODA.apply_command(command)
    if type(command) ~= "table" then return false, "command must be a table" end
    local action = command.action or command.type
    if not action then return false, "missing action" end

    if action == "observe" then return true, "observation refreshed" end
    if action == "skip_tutorial" then return command_skip_tutorial(command) end
    if action == "setup_new_run" then return command_setup_new_run(command) end
    if action == "start_run" then return command_start_run(command) end
    if action == "select_blind" then return command_select_blind(command) end
    if action == "skip_blind" then return command_skip_blind(command) end
    if action == "play" then return command_play(command) end
    if action == "discard" then return command_discard(command) end
    if action == "buy" then return command_buy(command, false) end
    if action == "buy_and_use" then return command_buy(command, true) end
    if action == "use" then return command_use(command) end
    if action == "sell" then return command_sell(command) end
    if action == "click" then return false, "coordinate click disabled; use semantic ui_click" end
    if action == "ui_click" then return command_ui_click(command) end
    if action == "ensure_menu_ui" then return command_ensure_menu_ui(command) end
    if action == "speed" then return command_speed(command) end
    if action == "sort_hand" then return command_sort_hand(command) end
    if action == "move_card" then return command_move_card(command) end
    if action == "move_joker" then return command_move_joker(command) end
    if action == "preview_hand" then return command_preview_hand(command) end
    if action == "reroll_shop" and G and G.FUNCS and G.FUNCS.reroll_shop then G.FUNCS.reroll_shop({}); return true, "queued reroll_shop" end
    if action == "next_round" then return command_next_round() end
    if action == "cash_out" then return command_cash_out() end
    if action == "skip_booster" and G and G.FUNCS and G.FUNCS.skip_booster then G.FUNCS.skip_booster({}); return true, "queued skip_booster" end
    if action == "reroll_boss" then return command_reroll_boss() end
    if action == "type_char" then
        local char = tostring(command.char or "")
        if not char or char == "" then return false, "char required" end
        if G and G.CONTROLLER and G.FUNCS and G.FUNCS.select_text_input then
            -- Focus the seed input field
            local seed_node = find_ui_node({ui_id = "run_select_seeded_input"}) or find_ui_node({button = "select_text_input"})
            if seed_node then seed_node:click() end
        end
        -- Type the character via text_input_key
        if G and G.CONTROLLER and G.FUNCS and G.FUNCS.text_input_key then
            G.FUNCS.text_input_key({key = char, caps = false})
        end
        return true, "typed: " .. char
    end

    return false, "unsupported or unavailable action: " .. tostring(action)
end

function CODA.decode_command(source)
    if not source or source:match("^%s*$") then return nil, "empty command file" end
    local chunk, err = loadstring(source)
    if not chunk then
        chunk, err = loadstring("return " .. source)
    end
    if not chunk then return nil, err end
    setfenv(chunk, {})
    local ok, result = pcall(chunk)
    if not ok then return nil, result end
    if type(result) ~= "table" then return nil, "command did not return a table" end
    return result, nil
end

local function write_decode_error(err_msg, original_source)
    CODA.response_seq = CODA.response_seq + 1
    write_response({
        ok = false,
        id = nil,
        _decode_error = true,
        message = "decode failed: " .. tostring(err_msg),
        version = CODA.version
    })
end

function CODA.poll_command()
    local source = love.filesystem.read(CODA.command_path)
    if not source or source == CODA.last_command_source then return end
    if type(source) == 'string' and source:len() == 0 then return end
    local command, err = CODA.decode_command(source)
    if err then
        CODA.last_command_source = source
        write_decode_error(err, source)
        return
    end

    local command_id = command.id or command.command_id
    if command_id and command_id == CODA.last_command_id then
        CODA.last_command_source = source
        return
    end

    CODA.last_command_source = source
    CODA.last_command_id = command_id
    local ok, success, message = pcall(function()
        local action_ok, action_message = CODA.apply_command(command)
        return action_ok, action_message
    end)
    if ok then
        write_response({
            ok = success,
            id = command_id,
            action = command.action or command.type,
            ui_id = command.ui_id,
            button = command.button,
            message = message,
            version = CODA.version,
            state = state_name()
        })
    else
        write_response({
            ok = false,
            id = command_id,
            action = command.action or command.type,
            ui_id = command.ui_id,
            button = command.button,
            message = tostring(success),
            version = CODA.version,
            state = state_name()
        })
    end
    CODA.write_observation()
end

function CODA.update(dt)
    dt = dt or 0
    CODA.poll_timer = (CODA.poll_timer or 0) + dt
    CODA.observe_timer = (CODA.observe_timer or 0) + dt

    if CODA.poll_timer >= CODA.poll_interval then
        CODA.poll_timer = 0
        pcall(CODA.poll_command)
    end

    if CODA.observe_timer >= CODA.observe_interval then
        CODA.observe_timer = 0
        CODA.write_observation()
    end
end

if not CODA.installed_update_hook then
    CODA.installed_update_hook = true
    local previous_love_update = love.update
    love.update = function(dt)
        if previous_love_update then previous_love_update(dt) end
        if CODA and CODA.update then CODA.update(dt) end
    end
end
