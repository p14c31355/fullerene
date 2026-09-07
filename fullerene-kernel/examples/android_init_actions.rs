//! Bounded Rust implementation of the trigger/action part of Android init.
//!
//! This is intentionally a small, data-driven subset.  It gives PID 1 an
//! ordered trigger boundary (`early-init`, `init`, and `boot`) without
//! pretending that the complete AOSP command grammar is already available.

pub const MAX_ACTIONS: usize = 8;
pub const MAX_COMMANDS: usize = 12;
pub const MAX_ARGS: usize = 6;

pub const CONFIG: &[u8] = b"# FullereneOS early Android actions\n\
on early-init\n\
    setprop sys.fullerene.init early-init\n\
on init\n\
    mount tmpfs /dev tmpfs 0\n\
    mount proc /proc proc 0\n\
    mount sysfs /sys sysfs 0\n\
    mount_all /etc/fstab.fullerene\n\
    mkdir /dev/socket 0755\n\
    write /sys/class/android_usb/state CONFIGURED\n\
    chmod /sys/class/android_usb/state 0644\n\
    chown 0 0 /sys/class/android_usb/state\n\
    setprop sys.fullerene.init init\n\
on boot\n\
    start fullerened\n\
    setprop sys.fullerene.init boot\n\
on property:sys.fullerene.trigger=restart\n\
    restart fullerened\n";

#[derive(Clone, Copy)]
pub struct InitCommand<'a> {
    pub name: &'a [u8],
    pub args: [Option<&'a [u8]>; MAX_ARGS],
    pub argc: usize,
}

#[derive(Clone, Copy)]
pub struct ActionSpec<'a> {
    pub trigger: &'a [u8],
    pub commands: [Option<InitCommand<'a>>; MAX_COMMANDS],
    pub command_count: usize,
}

#[derive(Clone, Copy)]
pub struct ActionTable<'a> {
    pub actions: [Option<ActionSpec<'a>>; MAX_ACTIONS],
    pub len: usize,
    pub valid: bool,
}

impl<'a> ActionTable<'a> {
    const fn empty() -> Self {
        Self {
            actions: [None; MAX_ACTIONS],
            len: 0,
            valid: true,
        }
    }

    pub fn find(&self, trigger: &[u8]) -> Option<(usize, ActionSpec<'a>)> {
        let mut index = 0;
        while index < self.len {
            if let Some(action) = self.actions[index]
                && action.trigger == trigger
            {
                return Some((index, action));
            }
            index += 1;
        }
        None
    }
}

/// Match the bounded AOSP property-trigger spelling without allocating or
/// normalizing the property name/value. Keeping this at the parser boundary
/// lets PID 1 evaluate every configured property action in source order.
pub fn property_trigger_matches(trigger: &[u8], name: &[u8], value: &[u8]) -> bool {
    const PREFIX: &[u8] = b"property:";
    if !trigger.starts_with(PREFIX) {
        return false;
    }
    let expression = &trigger[PREFIX.len()..];
    let Some(separator) = expression.iter().position(|byte| *byte == b'=') else {
        return false;
    };
    !name.is_empty() && &expression[..separator] == name && &expression[separator + 1..] == value
}

pub fn parse(data: &[u8]) -> ActionTable<'_> {
    let mut table = ActionTable::empty();
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
        if directive == b"on" {
            let trigger = trim(line.get(cursor..).unwrap_or_default());
            if trigger.is_empty() || table.len >= MAX_ACTIONS {
                table.valid = false;
                current = None;
                continue;
            }
            let action = ActionSpec {
                trigger,
                commands: [None; MAX_COMMANDS],
                command_count: 0,
            };
            table.actions[table.len] = Some(action);
            current = Some(table.len);
            table.len += 1;
            continue;
        }

        let Some(index) = current else {
            table.valid = false;
            continue;
        };
        let Some(mut action) = table.actions[index] else {
            table.valid = false;
            continue;
        };
        if !supported_command(directive) || action.command_count >= MAX_COMMANDS {
            table.valid = false;
            continue;
        }

        let mut command = InitCommand {
            name: directive,
            args: [None; MAX_ARGS],
            argc: 0,
        };
        while let Some(argument) = token(line, &mut cursor) {
            if command.argc >= MAX_ARGS {
                table.valid = false;
                break;
            }
            command.args[command.argc] = Some(argument);
            command.argc += 1;
        }
        if command_valid(command) {
            action.commands[action.command_count] = Some(command);
            action.command_count += 1;
            table.actions[index] = Some(action);
        } else {
            table.valid = false;
        }
    }

    table
}

pub fn configured() -> ActionTable<'static> {
    parse(CONFIG)
}

pub fn self_test() -> bool {
    let table = configured();
    if !table.valid || table.len != 4 {
        return false;
    }
    let Some((early_index, early)) = table.find(b"early-init") else {
        return false;
    };
    let Some((init_index, init)) = table.find(b"init") else {
        return false;
    };
    let Some((boot_index, boot)) = table.find(b"boot") else {
        return false;
    };
    let Some((property_index, property)) = table.find(b"property:sys.fullerene.trigger=restart")
    else {
        return false;
    };
    early_index == 0
        && init_index == 1
        && boot_index == 2
        && property_index == 3
        && command_is(
            early,
            0,
            b"setprop",
            &[b"sys.fullerene.init", b"early-init"],
        )
        && init.command_count == 9
        && command_is(init, 0, b"mount", &[b"tmpfs", b"/dev", b"tmpfs", b"0"])
        && command_is(init, 1, b"mount", &[b"proc", b"/proc", b"proc", b"0"])
        && command_is(init, 2, b"mount", &[b"sysfs", b"/sys", b"sysfs", b"0"])
        && command_is(init, 3, b"mount_all", &[b"/etc/fstab.fullerene"])
        && command_is(init, 4, b"mkdir", &[b"/dev/socket", b"0755"])
        && command_is(
            init,
            5,
            b"write",
            &[b"/sys/class/android_usb/state", b"CONFIGURED"],
        )
        && command_is(
            init,
            6,
            b"chmod",
            &[b"/sys/class/android_usb/state", b"0644"],
        )
        && command_is(
            init,
            7,
            b"chown",
            &[b"0", b"0", b"/sys/class/android_usb/state"],
        )
        && command_is(init, 8, b"setprop", &[b"sys.fullerene.init", b"init"])
        && command_is(boot, 0, b"start", &[b"fullerened"])
        && command_is(boot, 1, b"setprop", &[b"sys.fullerene.init", b"boot"])
        && command_is(property, 0, b"restart", &[b"fullerened"])
        && property_trigger_matches(
            b"property:sys.fullerene.trigger=restart",
            b"sys.fullerene.trigger",
            b"restart",
        )
        && !property_trigger_matches(
            b"property:sys.fullerene.trigger=restart",
            b"sys.fullerene.trigger",
            b"start",
        )
}

fn command_valid(command: InitCommand<'_>) -> bool {
    match command.name {
        b"setprop" => command.argc == 2,
        b"start" | b"stop" | b"restart" => command.argc == 1,
        b"mount" => command.argc == 4 || command.argc == 5,
        b"mount_all" => command.argc == 1 || command.argc == 2,
        b"mkdir" => command.argc == 2,
        b"write" => command.argc == 2,
        b"chmod" => command.argc == 2,
        b"chown" => command.argc == 3,
        b"wait" => command.argc == 1 || command.argc == 2,
        _ => false,
    }
}

fn command_is(action: ActionSpec<'_>, index: usize, name: &[u8], args: &[&[u8]]) -> bool {
    let Some(command) = action.commands.get(index).and_then(|command| *command) else {
        return false;
    };
    if command.name != name || command.argc != args.len() {
        return false;
    }
    let mut argument = 0;
    while argument < args.len() {
        if command.args[argument] != Some(args[argument]) {
            return false;
        }
        argument += 1;
    }
    true
}

fn supported_command(name: &[u8]) -> bool {
    matches!(
        name,
        b"setprop"
            | b"start"
            | b"stop"
            | b"restart"
            | b"mount"
            | b"mount_all"
            | b"mkdir"
            | b"write"
            | b"chmod"
            | b"chown"
            | b"wait"
    )
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
