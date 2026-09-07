//! Bounded Rust implementation of the service-stanza part of Android init.
//!
//! The production AOSP configuration is much larger than the early boot
//! surface needed by Fullerene.  Keeping this parser independent of the
//! process supervisor still gives the supervisor the same useful contract:
//! service names and executable paths are data, while credentials and
//! lifecycle flags are explicit fields rather than scattered conditionals.
//! The input is Rust-owned so the early image does not acquire a shell or a
//! text-generation dependency merely to describe its own services.

pub const MAX_SERVICES: usize = 4;
pub const MAX_GROUPS: usize = 4;

pub const CONFIG: &[u8] = b"# FullereneOS early Android services\n\
service fullerened /bin/fullerened\n\
    class core\n\
    user root\n\
    group root\n\
    seclabel u:r:fullerened:s0\n";

#[derive(Clone, Copy)]
pub struct ServiceSpec<'a> {
    pub name: &'a [u8],
    pub path: &'a [u8],
    pub class: &'a [u8],
    pub seclabel: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    pub groups: [u32; MAX_GROUPS],
    pub group_count: usize,
    pub disabled: bool,
    pub oneshot: bool,
    pub critical: bool,
}

#[derive(Clone, Copy)]
pub struct ServiceTable<'a> {
    pub specs: [Option<ServiceSpec<'a>>; MAX_SERVICES],
    pub len: usize,
    pub valid: bool,
}

impl<'a> ServiceTable<'a> {
    const fn empty() -> Self {
        Self {
            specs: [None; MAX_SERVICES],
            len: 0,
            valid: true,
        }
    }

    pub fn find(&self, name: &[u8]) -> Option<(usize, ServiceSpec<'a>)> {
        let mut index = 0;
        while index < self.len {
            if let Some(spec) = self.specs[index] {
                if spec.name == name {
                    return Some((index, spec));
                }
            }
            index += 1;
        }
        None
    }
}

pub fn parse(data: &[u8]) -> ServiceTable<'_> {
    let mut table = ServiceTable::empty();
    let mut current = None;
    let mut offset = 0;

    while offset <= data.len() {
        let end = data[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|length| offset + length)
            .unwrap_or(data.len());
        let line = trim(data.get(offset..end).unwrap_or_default());
        offset = if end < data.len() {
            end + 1
        } else {
            data.len() + 1
        };

        if line.is_empty() || line[0] == b'#' {
            continue;
        }

        let mut cursor = 0;
        let Some(directive) = token(line, &mut cursor) else {
            continue;
        };
        if directive == b"service" {
            let Some(name) = token(line, &mut cursor) else {
                table.valid = false;
                current = None;
                continue;
            };
            let Some(path) = token(line, &mut cursor) else {
                table.valid = false;
                current = None;
                continue;
            };
            if token(line, &mut cursor).is_some() || table.len >= MAX_SERVICES {
                table.valid = false;
                current = None;
                continue;
            }
            let spec = ServiceSpec {
                name,
                path,
                class: b"default",
                seclabel: &[],
                uid: 0,
                gid: 0,
                groups: [0; MAX_GROUPS],
                group_count: 0,
                disabled: false,
                oneshot: false,
                critical: false,
            };
            table.specs[table.len] = Some(spec);
            current = Some(table.len);
            table.len += 1;
            continue;
        }

        let Some(index) = current else {
            table.valid = false;
            continue;
        };
        let Some(mut spec) = table.specs[index] else {
            table.valid = false;
            continue;
        };
        match directive {
            b"class" => {
                let Some(value) = token(line, &mut cursor) else {
                    table.valid = false;
                    continue;
                };
                spec.class = value;
            }
            b"user" => {
                let Some(value) = token(line, &mut cursor) else {
                    table.valid = false;
                    continue;
                };
                let Some(uid) = android_id(value) else {
                    table.valid = false;
                    continue;
                };
                spec.uid = uid;
            }
            b"group" => {
                let mut count = 0;
                while let Some(value) = token(line, &mut cursor) {
                    if count >= MAX_GROUPS {
                        table.valid = false;
                        break;
                    }
                    let Some(gid) = android_id(value) else {
                        table.valid = false;
                        break;
                    };
                    spec.groups[count] = gid;
                    count += 1;
                }
                if count == 0 {
                    table.valid = false;
                }
                spec.group_count = count;
            }
            b"seclabel" => {
                let Some(value) = token(line, &mut cursor) else {
                    table.valid = false;
                    continue;
                };
                spec.seclabel = value;
            }
            b"disabled" => spec.disabled = true,
            b"oneshot" => spec.oneshot = true,
            b"critical" => spec.critical = true,
            // These directives affect later full init policy.  They are
            // intentionally accepted as metadata-neutral lines so one
            // service stanza can be extended without changing the parser's
            // memory layout.
            b"capabilities" | b"namespace" | b"socket" | b"file" => {}
            _ => table.valid = false,
        }
        table.specs[index] = Some(spec);
    }

    table
}

pub fn configured() -> ServiceTable<'static> {
    parse(CONFIG)
}

pub fn self_test() -> bool {
    let table = configured();
    if !table.valid || table.len != 1 {
        return false;
    }
    let Some((index, spec)) = table.find(b"fullerened") else {
        return false;
    };
    if index != 0
        || spec.path != b"/bin/fullerened"
        || spec.class != b"core"
        || spec.seclabel != b"u:r:fullerened:s0"
        || spec.uid != 0
        || spec.gid != 0
        || spec.group_count != 1
        || spec.groups[0] != 0
        || spec.disabled
        || spec.oneshot
        || spec.critical
    {
        return false;
    }

    let sample = b"service sample /bin/sample\n\
        class main\n\
        user 2000\n\
        group shell audio\n\
        disabled\n\
        oneshot\n\
        critical\n";
    let sample_table = parse(sample);
    let Some((sample_index, sample_spec)) = sample_table.find(b"sample") else {
        return false;
    };
    sample_table.valid
        && sample_table.len == 1
        && sample_index == 0
        && sample_spec.path == b"/bin/sample"
        && sample_spec.class == b"main"
        && sample_spec.uid == 2000
        && sample_spec.group_count == 2
        && sample_spec.groups[0] == 2000
        && sample_spec.groups[1] == 1005
        && sample_spec.disabled
        && sample_spec.oneshot
        && sample_spec.critical
}

fn trim(data: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = data.len();
    while start < end && data[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && data[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &data[start..end]
}

fn token<'a>(line: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    while *cursor < line.len() && line[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
    if *cursor >= line.len() || line[*cursor] == b'#' {
        return None;
    }
    let start = *cursor;
    while *cursor < line.len() && !line[*cursor].is_ascii_whitespace() && line[*cursor] != b'#' {
        *cursor += 1;
    }
    Some(&line[start..*cursor])
}

fn android_id(value: &[u8]) -> Option<u32> {
    match value {
        b"root" => Some(0),
        b"system" => Some(1000),
        b"radio" => Some(1001),
        b"graphics" => Some(1003),
        b"input" => Some(1004),
        b"audio" => Some(1005),
        b"camera" => Some(1006),
        b"log" => Some(1007),
        b"compass" => Some(1008),
        b"mount" => Some(1009),
        b"shell" => Some(2000),
        b"cache" => Some(2001),
        b"diag" => Some(2002),
        b"net_bt_admin" => Some(3001),
        b"net_bt" => Some(3002),
        b"net_bw_stats" => Some(3006),
        b"net_bw_acct" => Some(3007),
        b"readproc" => Some(3009),
        b"wakelock" => Some(3010),
        b"media" => Some(1013),
        b"nobody" => Some(9999),
        _ => decimal(value),
    }
}

fn decimal(value: &[u8]) -> Option<u32> {
    if value.is_empty() {
        return None;
    }
    let mut result = 0u32;
    for byte in value {
        if !byte.is_ascii_digit() {
            return None;
        }
        result = result
            .checked_mul(10)?
            .checked_add(u32::from(*byte - b'0'))?;
    }
    Some(result)
}
