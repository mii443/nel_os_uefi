pub(crate) const BANNER: &[u8] =
    b"nel hypervisor management shell\r\nType 'help' for commands.\r\nnel> ";
pub(crate) const PROMPT: &[u8] = b"nel> ";
pub(crate) const HELP: &[u8] = b"Commands:\r\n  vm list\r\n  vm create [ID] MEMORY\r\n  vm start [ID] [--attach|-a]\r\n  vm stop|reset|status [ID]\r\n  serial attach [ID]\r\n  serial detach\r\n  info memory|runtime|all\r\n  info vm [ID]\r\n  help\r\n  exit\r\nVMs are created dynamically. Omitting ID from 'vm create' selects the lowest free ID; other commands default to VM 0. MEMORY is MiB unless suffixed M/MiB/G/GiB.\r\n";

pub const DEFAULT_VM_ID: usize = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementCommand {
    VmList,
    VmCreate { id: Option<usize>, memory_mib: u32 },
    VmStart { id: usize, attach: bool },
    VmStop { id: usize },
    VmReset { id: usize },
    VmStatus { id: usize },
    SerialAttach { id: usize },
    SerialDetach,
    InfoMemory,
    InfoRuntime,
    InfoAll,
    Help,
    Prompt,
    Invalid,
    Disconnect,
}

impl ManagementCommand {
    pub(crate) fn vm_id(self) -> Option<usize> {
        match self {
            Self::VmStart { id, .. }
            | Self::VmStop { id }
            | Self::VmReset { id }
            | Self::VmStatus { id }
            | Self::SerialAttach { id } => Some(id),
            Self::VmCreate { id, .. } => id,
            _ => None,
        }
    }

    pub(crate) fn changes_vm_lifecycle(self) -> bool {
        matches!(self, Self::VmStop { .. } | Self::VmReset { .. })
    }
}

pub(crate) fn parse_command(line: &[u8]) -> ManagementCommand {
    let line = trim_ascii(line);
    let mut words = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return ManagementCommand::Invalid;
    };

    if eq(first, b"vm") {
        let Some(operation) = words.next() else {
            return ManagementCommand::Invalid;
        };
        if eq(operation, b"list") && words.next().is_none() {
            return ManagementCommand::VmList;
        }
        if eq(operation, b"create") {
            return parse_create(words);
        }
        if eq(operation, b"start") {
            return parse_start(words);
        }
        if eq(operation, b"stop") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::VmStop { id }
            });
        }
        if eq(operation, b"reset") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::VmReset { id }
            });
        }
        if eq(operation, b"status") || eq(operation, b"info") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::VmStatus { id }
            });
        }
        return ManagementCommand::Invalid;
    }

    if eq(first, b"start") {
        return parse_start(words);
    }
    if eq(first, b"stop") {
        return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
            ManagementCommand::VmStop { id }
        });
    }
    if eq(first, b"reset") {
        return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
            ManagementCommand::VmReset { id }
        });
    }
    if eq(first, b"list") && words.next().is_none() {
        return ManagementCommand::VmList;
    }

    if eq(first, b"serial") {
        let Some(operation) = words.next() else {
            return ManagementCommand::Invalid;
        };
        if eq(operation, b"attach") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::SerialAttach { id }
            });
        }
        if eq(operation, b"detach") && words.next().is_none() {
            return ManagementCommand::SerialDetach;
        }
        return ManagementCommand::Invalid;
    }

    if eq(first, b"info") {
        let Some(subject) = words.next() else {
            return ManagementCommand::InfoAll;
        };
        if eq(subject, b"memory") && words.next().is_none() {
            return ManagementCommand::InfoMemory;
        }
        if eq(subject, b"runtime") && words.next().is_none() {
            return ManagementCommand::InfoRuntime;
        }
        if eq(subject, b"all") && words.next().is_none() {
            return ManagementCommand::InfoAll;
        }
        if eq(subject, b"vm") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::VmStatus { id }
            });
        }
        return ManagementCommand::Invalid;
    }

    if (eq(first, b"help") || first == b"?") && words.next().is_none() {
        ManagementCommand::Help
    } else if (eq(first, b"exit") || eq(first, b"quit")) && words.next().is_none() {
        ManagementCommand::Disconnect
    } else {
        ManagementCommand::Invalid
    }
}

fn parse_start<'a>(words: impl Iterator<Item = &'a [u8]>) -> ManagementCommand {
    let mut id = DEFAULT_VM_ID;
    let mut saw_id = false;
    let mut attach = false;

    for word in words {
        if eq(word, b"--attach") || eq(word, b"-a") {
            if attach {
                return ManagementCommand::Invalid;
            }
            attach = true;
        } else if !saw_id {
            let Some(parsed) = parse_usize(word) else {
                return ManagementCommand::Invalid;
            };
            id = parsed;
            saw_id = true;
        } else {
            return ManagementCommand::Invalid;
        }
    }

    ManagementCommand::VmStart { id, attach }
}

fn parse_create<'a>(mut words: impl Iterator<Item = &'a [u8]>) -> ManagementCommand {
    let Some(first) = words.next() else {
        return ManagementCommand::Invalid;
    };
    let second = words.next();
    if words.next().is_some() {
        return ManagementCommand::Invalid;
    }

    let (id, memory) = match second {
        Some(memory) => {
            let Some(id) = parse_usize(first) else {
                return ManagementCommand::Invalid;
            };
            (Some(id), memory)
        }
        None => (None, first),
    };
    let Some(memory_mib) = parse_memory_mib(memory) else {
        return ManagementCommand::Invalid;
    };
    ManagementCommand::VmCreate { id, memory_mib }
}

fn parse_vm_id<'a>(mut words: impl Iterator<Item = &'a [u8]>) -> Option<usize> {
    let id = match words.next() {
        Some(word) => parse_usize(word)?,
        None => DEFAULT_VM_ID,
    };
    words.next().is_none().then_some(id)
}

fn parse_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0usize;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
    }
    Some(value)
}

fn parse_memory_mib(bytes: &[u8]) -> Option<u32> {
    let (digits, multiplier) = if let Some(digits) = strip_suffix(bytes, b"mib") {
        (digits, 1)
    } else if let Some(digits) = strip_suffix(bytes, b"mb") {
        (digits, 1)
    } else if let Some(digits) = strip_suffix(bytes, b"m") {
        (digits, 1)
    } else if let Some(digits) = strip_suffix(bytes, b"gib") {
        (digits, 1024)
    } else if let Some(digits) = strip_suffix(bytes, b"gb") {
        (digits, 1024)
    } else if let Some(digits) = strip_suffix(bytes, b"g") {
        (digits, 1024)
    } else {
        (bytes, 1)
    };

    if digits.is_empty() {
        return None;
    }
    let mut value = 0u32;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((byte - b'0') as u32)?;
    }
    value.checked_mul(multiplier)
}

fn strip_suffix<'a>(bytes: &'a [u8], suffix: &[u8]) -> Option<&'a [u8]> {
    if bytes.len() < suffix.len()
        || !bytes[bytes.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        return None;
    }
    Some(&bytes[..bytes.len() - suffix.len()])
}

fn eq(left: &[u8], right: &[u8]) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_vm_aliases_and_attach_option() {
        assert_eq!(
            parse_command(b"start"),
            ManagementCommand::VmStart {
                id: 0,
                attach: false
            }
        );
        assert_eq!(
            parse_command(b" VM START -a "),
            ManagementCommand::VmStart {
                id: 0,
                attach: true
            }
        );
        assert_eq!(
            parse_command(b"vm info"),
            ManagementCommand::VmStatus { id: 0 }
        );
        assert_eq!(
            parse_command(b"info vm"),
            ManagementCommand::VmStatus { id: 0 }
        );
        assert_eq!(parse_command(b"?"), ManagementCommand::Help);
    }

    #[test]
    fn parses_explicit_vm_ids() {
        assert_eq!(
            parse_command(b"vm create 2 128MiB"),
            ManagementCommand::VmCreate {
                id: Some(2),
                memory_mib: 128
            }
        );
        assert_eq!(
            parse_command(b"vm create 1G"),
            ManagementCommand::VmCreate {
                id: None,
                memory_mib: 1024
            }
        );
        assert_eq!(
            parse_command(b"vm create 4096 128M"),
            ManagementCommand::VmCreate {
                id: Some(4096),
                memory_mib: 128
            }
        );
        assert_eq!(
            parse_command(b"vm start 3 --attach"),
            ManagementCommand::VmStart {
                id: 3,
                attach: true
            }
        );
        assert_eq!(
            parse_command(b"vm start -a 2"),
            ManagementCommand::VmStart {
                id: 2,
                attach: true
            }
        );
        assert_eq!(
            parse_command(b"vm stop 1"),
            ManagementCommand::VmStop { id: 1 }
        );
        assert_eq!(
            parse_command(b"serial attach 2"),
            ManagementCommand::SerialAttach { id: 2 }
        );
        assert_eq!(parse_command(b"vm list"), ManagementCommand::VmList);
    }

    #[test]
    fn rejects_malformed_or_overflowing_ids() {
        assert_eq!(parse_command(b"vm start 1 2"), ManagementCommand::Invalid);
        assert_eq!(parse_command(b"vm stop -1"), ManagementCommand::Invalid);
        assert_eq!(
            parse_command(b"vm reset 18446744073709551616"),
            ManagementCommand::Invalid
        );
    }
}
