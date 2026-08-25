"""config_parser.py — CORRECT reference fix (the expected end state).

Diffs against the buggy version define task_pass:
  - validate_config returns False if any required key missing (else True)
  - parse_config tolerates empty/non-numeric values (stores as string, not int)
  - get_int coerces; raises ValueError if present-but-non-numeric (not silent default)
"""


def parse_config(lines):
    config = {}
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip()
        # FIX 2: do not coerce to int here — keep the raw string.
        try:
            value = int(value)
        except ValueError:
            pass
        config[key.strip()] = value
    return config


def validate_config(config):
    required = ["host", "port", "timeout"]
    ok = True
    for key in required:
        if key not in config:
            print("Missing required key: %s" % key)
            ok = False
    # FIX 1: return False if any required key is missing.
    return ok


def get_int(config, key, default=0):
    val = config.get(key)
    # FIX 3: present-but-non-numeric must raise, not silently default.
    if val is None:
        return default
    if isinstance(val, int):
        return val
    return int(val)
