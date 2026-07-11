import json, sqlite3, sys, os

DB_PATH = os.path.join(os.path.dirname(__file__), 'replays.db')

def get_db():
    return sqlite3.connect(DB_PATH)

CLEAR = 'clear'
FAIL = 'fail'

def init_db(conn):
    c = conn.cursor()
    c.execute('''CREATE TABLE IF NOT EXISTS replay (
        id INTEGER PRIMARY KEY, seed TEXT NOT NULL, ante INTEGER NOT NULL,
        stake INTEGER NOT NULL, blind_key TEXT NOT NULL, outcome TEXT NOT NULL,
        chips_required INTEGER, max_chips_gained INTEGER,
        created_at TEXT DEFAULT (datetime('now')))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_step (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, step_order INTEGER,
        action_type TEXT NOT NULL, details TEXT NOT NULL, rationale TEXT,
        hand_type TEXT, cards_held TEXT, cards_discarded TEXT, discard_count INTEGER DEFAULT 0,
        final_cards TEXT, base_chips INTEGER, base_mult INTEGER, final_score INTEGER,
        consumable_name TEXT, consumable_target_hand TEXT, notes TEXT,
        FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_joker_config (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, slot_order INTEGER NOT NULL,
        joker_name TEXT NOT NULL, edition TEXT, enhancement TEXT, notes TEXT,
        FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_hand_levels (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, hand_type TEXT NOT NULL,
        level INTEGER NOT NULL, chips INTEGER, mult INTEGER,
        FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_voucher (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, voucher_name TEXT NOT NULL,
        slot_order INTEGER, FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_economy (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, dollars_start INTEGER,
        dollars_end INTEGER, shop_items_bought TEXT, shop_items_skipped TEXT,
        FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    c.execute('''CREATE TABLE IF NOT EXISTS replay_tags (
        id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, tag_name TEXT NOT NULL,
        source TEXT, FOREIGN KEY (replay_id) REFERENCES replay(id))''')
    conn.commit()

def safe_int(val):
    """Convert a value to int, returning 0 for None/empty/invalid."""
    try:
        return int(val) if val else 0
    except (TypeError, ValueError):
        return 0


def _parse_steps(steps_str):
    """Parse semicolon-separated steps into structured dicts.

    Each step uses pipe-delimited fields:
      action_type|details|rationale|hand_type|cards_held|cards_discarded
      |discard_count|final_cards|base_chips|base_mult|final_score
      |consumable_name|consumable_target_hand|notes

    Rationale (field 2) may contain pipes in the new format; we handle this
    by joining any overflow fields back into rationale for backwards compat.
    """
    results = []
    if not steps_str:
        return results
    if isinstance(steps_str, list):
        steps_str = ';'.join(str(s) for s in steps_str if s)
    if not str(steps_str).strip():
        return results

    for entry in str(steps_str).split(';'):
        entry = entry.strip()
        if not entry:
            continue
        parts = [p.strip() for p in entry.split('|')]

        # Handle overflow: if more than 14 fields, merge extras into notes
        if len(parts) > 14:
            extra = '|'.join(parts[14:]).strip()
            if extra:
                parts[13] = (parts[13] or '') + '; ' + extra
            parts = parts[:14]

        while len(parts) < 14:
            parts.append(None)

        results.append({
            'action_type': parts[0] or '',
            'details': parts[1] if parts[1] else None,
            'rationale': parts[2] if parts[2] else None,
            'hand_type': parts[3] if parts[3] else None,
            'cards_held': parts[4] if parts[4] else None,
            'cards_discarded': parts[5] if parts[5] else None,
            'discard_count': safe_int(parts[6]),
            'final_cards': parts[7] if parts[7] else None,
            'base_chips': safe_int(parts[8]),
            'base_mult': safe_int(parts[9]),
            'final_score': safe_int(parts[10]),
            'consumable_name': parts[11] if parts[11] else None,
            'consumable_target_hand': parts[12] if parts[12] else None,
            'notes': parts[13] if parts[13] else None,
        })
    return results

def _load_replay_detail(c, replay_id):
    """Load all sub-tables for a replay into a dict."""
    c.execute('SELECT slot_order, joker_name, edition, enhancement, notes FROM replay_joker_config WHERE replay_id=? ORDER BY slot_order', (replay_id,))
    jokers = [{'slot_order': s, 'joker_name': n, 'edition': e, 'enhancement': h, 'notes': nt} for s, n, e, h, nt in c.fetchall()]
    c.execute('SELECT slot_order, voucher_name FROM replay_voucher WHERE replay_id=? ORDER BY slot_order', (replay_id,))
    vouchers = [{'slot_order': s, 'voucher_name': v} for s, v in c.fetchall()]
    c.execute('SELECT hand_type, level, chips, mult FROM replay_hand_levels WHERE replay_id=? ORDER BY hand_type', (replay_id,))
    hand_levels = [{'hand_type': h, 'level': l, 'chips': c_, 'mult': m} for h, l, c_, m in c.fetchall()]
    c.execute('SELECT step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes FROM replay_step WHERE replay_id=? ORDER BY step_order', (replay_id,))
    steps = [{'step_order': s, 'action_type': a, 'details': d, 'rationale': r, 'hand_type': h, 'cards_held': ch, 'cards_discarded': cd, 'discard_count': dc, 'final_cards': fc, 'base_chips': bc, 'base_mult': bm, 'final_score': fs, 'consumable_name': cn, 'consumable_target_hand': th, 'notes': nt} for s, a, d, r, h, ch, cd, dc, fc, bc, bm, fs, cn, th, nt in c.fetchall()]
    c.execute('SELECT dollars_start, dollars_end, shop_items_bought, shop_items_skipped FROM replay_economy WHERE replay_id=?', (replay_id,))
    eco = c.fetchone()
    economy = {'dollars_start': eco[0], 'dollars_end': eco[1], 'shop_items_bought': eco[2], 'shop_items_skipped': eco[3]} if eco and eco[0] is not None else None
    c.execute('SELECT tag_name FROM replay_tags WHERE replay_id=?', (replay_id,))
    tags = [t[0] for t in c.fetchall()]
    return {'jokers': jokers, 'vouchers': vouchers, 'hand_levels': hand_levels, 'steps': steps, 'economy': economy, 'tags': tags}

def format_replay(c, replay):
    rid, rseed, rante, rstake, rblind, routcome, creq, cgained = replay[:9]
    print('=' * 70)
    if routcome == CLEAR:
        print(f'REPLAY #{rid}: {rblind} (Ante {rante}, Stake {rstake}) - CLEARED')
    else:
        print(f'REPLAY #{rid}: {rblind} (Ante {rante}, Stake {rstake}) - FAILED')
    print(f'  Seed: {rseed}')
    if routcome == CLEAR and creq is not None:
        print(f'  Required: {creq} chips | Achieved: {cgained} chips')
    c.execute('SELECT slot_order, joker_name, edition, enhancement, notes FROM replay_joker_config WHERE replay_id=? ORDER BY slot_order', (rid,))
    for sord, jname, edition, enh, notes in c.fetchall():
        parts = [jname]
        if edition: parts.append(edition)
        if enh: parts.append(enh)
        line = f'    Slot {sord}: ' + ', '.join(parts)
        if notes: line += f' | {notes}'
        print(line)
    c.execute('SELECT slot_order, voucher_name FROM replay_voucher WHERE replay_id=? ORDER BY slot_order', (rid,))
    for sord, vname in c.fetchall():
        print(f'    Slot {sord}: {vname}')
    c.execute('SELECT hand_type, level, chips, mult FROM replay_hand_levels WHERE replay_id=? ORDER BY hand_type', (rid,))
    for ht, lvl, ch, mu in c.fetchall():
        print(f'    {ht}: Level {lvl} ({ch} chips, {mu}x mult)')
    c.execute('''SELECT step_order, action_type, details, rationale,
                    hand_type, cards_held, cards_discarded, discard_count, final_cards,
                    base_chips, base_mult, final_score, consumable_name,
                    consumable_target_hand, notes
             FROM replay_step WHERE replay_id=? ORDER BY step_order''', (rid,))
    for sord, atype, details, rationale, ht, cheld, cdisc, dcount, fcards, bchips, bmult, fscore, cname, thand, notes in c.fetchall():
        line = f'    Step {sord}: [{atype}] {details}'
        print(line)
        if rationale: print(f'      Rationale: {rationale}')
        if ht and cheld:
            print(f'      Hand: {ht} | Kept: {cheld}', end='')
            if cdisc: print(f' | Discarded: {cdisc}', end='')
            if dcount: print(f' ({dcount} discards)', end='')
            if fcards: print(f' | Final: {fcards}', end='')
            if bchips and bmult: print(f' | Base: {bchips}/{bmult}', end='')
            if fscore: print(f' | Score: {fscore}', end='')
            print()
        elif ht:
            print(f'      Hand: {ht}', end='')
            if fcards: print(f' | Final: {fcards}', end='')
            if fscore: print(f' | Score: {fscore}', end='')
            print()
        if cname: print(f'      Consumable: {cname} -> target hand: {thand}')
        if notes: print(f'      Notes: {notes}')
    c.execute('SELECT dollars_start, dollars_end, shop_items_bought, shop_items_skipped FROM replay_economy WHERE replay_id=?', (rid,))
    eco = c.fetchone()
    if eco and eco[0] is not None:
        ds, de, sb, ss = eco
        print(f'\n  ECONOMY:  -> ')
        if sb: print(f'    Bought: {sb}')
        if ss: print(f'    Skipped: {ss}')
    c.execute('SELECT tag_name FROM replay_tags WHERE replay_id=?', (rid,))
    for tname in c.fetchall(): print(f'    Tag: {tname[0]}')

def format_replay_json(replay, details):
    rid, rseed, rante, rstake, rblind, routcome, creq, cgained = replay[:9]
    return {
        'id': rid, 'seed': rseed, 'ante': rante, 'stake': rstake,
        'blind_key': rblind, 'outcome': routcome,
        'chips_required': creq, 'max_chips_gained': cgained,
        'jokers': details['jokers'], 'vouchers': details['vouchers'],
        'hand_levels': details['hand_levels'], 'steps': details['steps'],
        'economy': details['economy'], 'tags': details['tags'],
    }

def query_replay(args):
    json_mode = '--json' in args
    conn = get_db(); init_db(conn); c = conn.cursor()
    seed=None; ante=None; stake=None; blind=None; outcome_filter=None
    i = 0
    while i < len(args):
        arg = args[i]
        if arg.startswith('@seed:'): seed = arg[6:]
        elif arg.startswith('@ante:'): ante = int(arg[6:])
        elif arg.startswith('@stake:'): stake = int(arg[7:])
        elif arg.startswith('@blind:'): blind = arg[7:]
        elif arg == '@clear': outcome_filter = CLEAR
        elif arg == '@fail': outcome_filter = FAIL
        i += 1
    query = 'SELECT id, seed, ante, stake, blind_key, outcome, chips_required, max_chips_gained FROM replay WHERE 1=1'
    params = []
    if seed: query += ' AND seed = ?'; params.append(seed)
    if ante is not None: query += ' AND ante = ?'; params.append(ante)
    if stake is not None: query += ' AND stake = ?'; params.append(stake)
    if blind: query += ' AND blind_key LIKE ?'; params.append(f'%{blind}%')
    if outcome_filter: query += ' AND outcome = ?'; params.append(outcome_filter)
    c.execute(query, params); replays = c.fetchall()
    if not replays: print('No replays found.'); conn.close(); return
    if json_mode:
        results = []
        for replay in replays:
            details = _load_replay_detail(c, replay[0])
            results.append(format_replay_json(replay, details))
        print(json.dumps(results, indent=2))
    else:
        for replay in replays: format_replay(c, replay)
    conn.close()

def query_best_replay(args):
    json_mode = '--json' in args
    conn = get_db(); init_db(conn); c = conn.cursor()
    conditions=[]; params=[]
    for arg in args:
        if ':' not in arg: continue
        key, val = arg.split(':', 1)
        if key == '@seed': conditions.append('r.seed=?'); params.append(val)
        elif key == '@ante': conditions.append('r.ante=?'); params.append(int(val))
        elif key == '@stake': conditions.append('r.stake=?'); params.append(int(val))
        elif key == '@blind': conditions.append('r.blind_key LIKE ?'); params.append(f'%{val}%')
    where = ' WHERE ' + ' AND '.join(conditions) if conditions else ''
    query = f'SELECT id, seed, ante, stake, blind_key, outcome, chips_required, max_chips_gained FROM replay r{where} ORDER BY max_chips_gained DESC LIMIT 1'
    c.execute(query, params); replay = c.fetchone()
    if not replay: print('No replays found.'); conn.close(); return
    if json_mode:
        details = _load_replay_detail(c, replay[0])
        print(json.dumps(format_replay_json(replay, details), indent=2))
    else:
        format_replay(c, replay)
    conn.close()

def log_clear(args):
    conn = get_db(); init_db(conn); c = conn.cursor()
    if len(args) < 4:
        print('Usage: python replays.py clear <seed> <ante> <stake> <blind_key> [options]')
        print('Options (key:value, space-separated):')
        print('  jokers:name1,name2,...')
        print('  joker_N:N:edition:enhancement:notes   (N=slot number)')
        print('  vouchers:v1,v2,... | hand_levels:H=Lvl(C,M);H2=Lvl2(C2,M2)')
        print('  dollars_start: | dollars_end:')
        print('  shop_bought:item1,item2 | shop_skipped:item1,item2')
        print('  steps:ACTION|DETAILS|RATIONALE|HAND_TYPE|CARDS_HELD|CARDS_DISCARDED|DISCARD_COUNT|FINAL_CARDS|BASE_CHIPS|BASE_MULT|SCORE|CONSUMABLE|TARGET_HAND|NOTES;...')
        print('    Fields separated by |. Empty fields must still have a | separator.')
        print('    Example: play_hand|Play 3-of-a-kind|Strong hand with Raised Fist|Three of a Kind|Q-Q-Q-7-2||||||850||||')
        print('  tags:t1,t2,... | notes:text')
        conn.close(); return
    seed, ante, stake, blind_key = args[0], int(args[1]), int(args[2]), args[3]
    joker_configs=[]; vouchers_list=[]; hand_levels_data=[]
    dollars_start=None; dollars_end=None; shop_bought=''; shop_skipped=''
    steps_raw=[]; tags_data=[]; notes_text=''; jokers_names=[]
    i = 4
    while i < len(args):
        arg = args[i]
        if ':' not in arg: i += 1; continue
        key, val = arg.split(':', 1)
        if key == 'jokers': jokers_names = [x.strip() for x in val.split(',')]
        elif key.startswith('joker_'):
            try: slot = int(key[6:])
            except ValueError: i += 1; continue
            parts = [p.strip() for p in val.split(':')]
            name = parts[0] if len(parts) > 0 else ''
            edition = parts[1] if len(parts) > 1 and parts[1] not in ('','N') else None
            enh = parts[2] if len(parts) > 2 and parts[2] not in ('','N') else None
            notes = parts[3] if len(parts) > 3 and parts[3] not in ('','N') else None
            joker_configs.append((slot, name, edition, enh, notes))
        elif key == 'vouchers': vouchers_list = [x.strip() for x in val.split(',')]
        elif key == 'hand_levels':
            hand_levels_data = []
            for hl in val.split(';'):
                hl = hl.strip()
                if not hl or '=' not in hl: continue
                eq_idx = hl.index('=')
                hname = hl[:eq_idx].strip()
                rest = hl[eq_idx+1:].strip()
                chips_val=None; mult_val=None; lvl_val=None
                if '(' in rest and ')' in rest:
                    po = rest.index('('); pc = rest.rindex(')')
                    cm_part = rest[po+1:pc]
                    nums = []
                    for cp in cm_part.split(','):
                        cp = cp.strip()
                        if not cp: continue
                        try: nums.append(int(cp))
                        except ValueError: pass
                    if len(nums) >= 3: lvl_val=nums[0]; chips_val=nums[1]; mult_val=nums[2]
                    elif len(nums) == 2: lvl_val=nums[0]; chips_val=nums[1]
                else:
                    try: lvl_val = int(rest.strip())
                    except ValueError: pass
                if lvl_val is not None: hand_levels_data.append((hname, lvl_val, chips_val, mult_val))
        elif key == 'dollars_start': dollars_start = int(val.replace('$',''))
        elif key == 'dollars_end': dollars_end = int(val.replace('$',''))
        elif key == 'shop_bought': shop_bought = val
        elif key == 'shop_skipped': shop_skipped = val
        elif key == 'steps': steps_raw = [x.strip() for x in val.split(';') if x.strip()]
        elif key == 'tags': tags_data = [t.strip() for t in val.split(',')]
        elif key == 'notes': notes_text = val
        i += 1
    c.execute('INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)',
              (seed, ante, stake, blind_key, CLEAR))
    rid = c.lastrowid
    parsed_steps = _parse_steps(steps_raw)
    for step in parsed_steps:
        c.execute('''INSERT INTO replay_step
            (replay_id, action_type, details, rationale, hand_type, cards_held,
             cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score,
             consumable_name, consumable_target_hand, notes)
            VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)''',
            (rid, step['action_type'], step['details'], step['rationale'], step['hand_type'], step['cards_held'],
             step['cards_discarded'], step['discard_count'], step['final_cards'],
             step['base_chips'], step['base_mult'], step['final_score'],
             step['consumable_name'], step['consumable_target_hand'], step['notes']))
    c.execute('SELECT id FROM replay_step WHERE replay_id=? ORDER BY rowid', (rid,))
    for idx, (sid,) in enumerate(c.fetchall(), 1):
        c.execute('UPDATE replay_step SET step_order=? WHERE id=?', (idx, sid))
    for slot, name, edition, enh, notes in joker_configs:
        c.execute('INSERT INTO replay_joker_config (replay_id,slot_order,joker_name,edition,enhancement,notes) VALUES (?,?,?,?,?,?)',
                  (rid, slot, name, edition, enh, notes))
    for idx, jname in enumerate(jokers_names, 1):
        c.execute('INSERT INTO replay_joker_config (replay_id,slot_order,joker_name) VALUES (?,?,?)',
                  (rid, idx, jname))
    for vname in vouchers_list:
        sord = vouchers_list.index(vname)+1
        c.execute('INSERT INTO replay_voucher (replay_id,slot_order,voucher_name) VALUES (?,?,?)', (rid, sord, vname))
    for hname, lvl, chips, mult in hand_levels_data:
        c.execute('INSERT INTO replay_hand_levels (replay_id,hand_type,level,chips,mult) VALUES (?,?,?,?,?)',
                  (rid, hname, lvl, chips, mult))
    if dollars_start is not None or dollars_end is not None:
        c.execute('INSERT INTO replay_economy (replay_id,dollars_start,dollars_end,shop_items_bought,shop_items_skipped) VALUES (?,?,?,?,?)',
                  (rid, dollars_start, dollars_end, shop_bought, shop_skipped))
    for tname in tags_data:
        c.execute('INSERT INTO replay_tags (replay_id,tag_name) VALUES (?,?)', (rid, tname))
    conn.commit()
    print(f'Replay #{rid} logged: Seed={seed}, Ante={ante}, Stake={stake}, {blind_key}')

def log_fail(args):
    conn = get_db(); init_db(conn); c = conn.cursor()
    if len(args) < 4:
        print('Usage: python replays.py fail <seed> <ante> <stake> <blind_key>')
        conn.close(); return
    seed, ante, stake, blind_key = args[0], int(args[1]), int(args[2]), args[3]
    c.execute('INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)',
              (seed, ante, stake, blind_key, FAIL))
    rid = c.lastrowid; conn.commit()
    print(f'Replay #{rid} logged (FAIL): Seed={seed}, Ante={ante}, {blind_key}')
    conn.close()

if __name__ == '__main__':
    capability = os.environ.get('BALATRO_MCP_CAPABILITY', '')
    capability_file = os.environ.get('BALATRO_MCP_CAPABILITY_FILE', '')
    try:
        with open(capability_file, encoding='utf-8') as handle:
            expected = handle.read().strip()
    except OSError:
        expected = ''
    if not capability or capability != expected:
        print(json.dumps({'ok': False, 'message': 'replay helper is private; use the balatro MCP tools'}), file=sys.stderr)
        raise SystemExit(2)
    args = sys.argv[1:]
    if not args:
        print('Usage:')
        print('  python replays.py @seed:2K9H9HN @ante:1 @clear')
        print('  python replays.py @seed:2K9H9HN @ante:1 @best')
        print('  python replays.py @seed:2K9H9HN @ante:1 @clear --json')
        print('  python replays.py @seed:2K9H9HN @ante:1 @best --json')
        print()
        print('Log clear: python replays.py clear <seed> <ante> <stake> <blind_key> [options]')
        print('Log fail:  python replays.py fail <seed> <ante> <stake> <blind_key>')
    elif args[0] == 'clear': log_clear(args[1:])
    elif args[0] == 'fail': log_fail(args[1:])
    elif '@best' in args: query_best_replay(args)
    else: query_replay(args)
