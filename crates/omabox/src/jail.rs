//! ai-jail (finding 99): a jailed agent's commands reach this omabox through the broker, which sets
//! OMABOX_JAIL, the jail's policy, for them. As far as box selection needs it.

use crate::json;
use crate::shell;
use serde_json::Value;

pub fn in_jail() -> bool {
    !shell::env_or_empty("OMABOX_JAIL").is_empty()
}

/// `jail_get .id`: the jail's id, as `jq -r .id` prints it ("null" for none).
pub fn jail_id() -> Vec<u8> {
    let (vals, _) = json::values(&shell::env_or_empty("OMABOX_JAIL"));
    let mut out = Vec::new();
    for v in &vals {
        let id = match v {
            Value::Object(m) => m.get("id").cloned().unwrap_or(Value::Null),
            Value::Null => Value::Null,
            _ => continue,
        };
        out.extend_from_slice(json::raw(&id).as_bytes());
        out.push(b'\n');
    }
    shell::strip_nl(&out)
}
