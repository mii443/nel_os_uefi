pub(crate) const BANNER: &[u8] =
    b"nel hypervisor management shell\r\nType 'help' for commands.\r\nnel> ";
pub(crate) const PROMPT: &[u8] = b"nel> ";
pub(crate) const HELP: &[u8] = b"Commands:\r\n  vm list\r\n  vm create [ID] MEMORY [--disk DISK]\r\n  vm start [ID] [--disk DISK] [--attach|-a]\r\n  vm stop|reset|delete|status [ID]\r\n  disk list\r\n  disk attach VM DISK\r\n  disk detach VM\r\n  serial attach [ID]\r\n  serial detach\r\n  info memory|runtime|all\r\n  info vm [ID]\r\n  help\r\n  exit\r\nVMs are created dynamically. Omitting ID from 'vm create' selects the lowest free ID; other commands default to VM 0. MEMORY is MiB unless suffixed M/MiB/G/GiB. Disks are host virtio-blk indices and may be changed only while a VM is not running.\r\n";

pub const DEFAULT_VM_ID: usize = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementCommand {
    VmList,
    VmCreate {
        id: Option<usize>,
        memory_mib: u32,
        disk: Option<usize>,
    },
    VmStart {
        id: usize,
        attach: bool,
        disk: Option<usize>,
    },
    VmStop {
        id: usize,
    },
    VmReset {
        id: usize,
    },
    VmDelete {
        id: usize,
    },
    VmStatus {
        id: usize,
    },
    DiskList,
    DiskAttach {
        id: usize,
        disk: usize,
    },
    DiskDetach {
        id: usize,
    },
    SerialAttach {
        id: usize,
    },
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
            | Self::VmDelete { id }
            | Self::VmStatus { id }
            | Self::DiskAttach { id, .. }
            | Self::DiskDetach { id }
            | Self::SerialAttach { id } => Some(id),
            Self::VmCreate { id, .. } => id,
            _ => None,
        }
    }

    pub(crate) fn changes_vm_lifecycle(self) -> bool {
        matches!(
            self,
            Self::VmStop { .. } | Self::VmReset { .. } | Self::VmDelete { .. }
        )
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
        if eq(operation, b"disk") {
            return parse_disk(words);
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
        if eq(operation, b"delete") || eq(operation, b"remove") {
            return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
                ManagementCommand::VmDelete { id }
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
    if eq(first, b"delete") || eq(first, b"remove") {
        return parse_vm_id(words).map_or(ManagementCommand::Invalid, |id| {
            ManagementCommand::VmDelete { id }
        });
    }
    if eq(first, b"list") && words.next().is_none() {
        return ManagementCommand::VmList;
    }

    if eq(first, b"disk") {
        return parse_disk(words);
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
    let mut words = words.peekable();
    let mut id = DEFAULT_VM_ID;
    let mut saw_id = false;
    let mut attach = false;
    let mut disk = None;

    while let Some(word) = words.next() {
        if eq(word, b"--attach") || eq(word, b"-a") {
            if attach {
                return ManagementCommand::Invalid;
            }
            attach = true;
        } else if eq(word, b"--disk") || eq(word, b"-d") {
            if disk.is_some() {
                return ManagementCommand::Invalid;
            }
            let Some(value) = words.next().and_then(parse_usize) else {
                return ManagementCommand::Invalid;
            };
            disk = Some(value);
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

    ManagementCommand::VmStart { id, attach, disk }
}

fn parse_create<'a>(words: impl Iterator<Item = &'a [u8]>) -> ManagementCommand {
    let mut words = words.peekable();
    let mut first = None;
    let mut second = None;
    let mut disk = None;
    while let Some(word) = words.next() {
        if eq(word, b"--disk") || eq(word, b"-d") {
            if disk.is_some() {
                return ManagementCommand::Invalid;
            }
            let Some(value) = words.next().and_then(parse_usize) else {
                return ManagementCommand::Invalid;
            };
            disk = Some(value);
        } else if first.is_none() {
            first = Some(word);
        } else if second.is_none() {
            second = Some(word);
        } else {
            return ManagementCommand::Invalid;
        }
    }
    let Some(first) = first else {
        return ManagementCommand::Invalid;
    };

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
    ManagementCommand::VmCreate {
        id,
        memory_mib,
        disk,
    }
}

fn parse_disk<'a>(mut words: impl Iterator<Item = &'a [u8]>) -> ManagementCommand {
    let Some(operation) = words.next() else {
        return ManagementCommand::Invalid;
    };
    if eq(operation, b"list") && words.next().is_none() {
        return ManagementCommand::DiskList;
    }
    if eq(operation, b"attach") {
        let (Some(id), Some(disk)) = (
            words.next().and_then(parse_usize),
            words.next().and_then(parse_usize),
        ) else {
            return ManagementCommand::Invalid;
        };
        return if words.next().is_none() {
            ManagementCommand::DiskAttach { id, disk }
        } else {
            ManagementCommand::Invalid
        };
    }
    if eq(operation, b"detach") {
        let Some(id) = words.next().and_then(parse_usize) else {
            return ManagementCommand::Invalid;
        };
        return if words.next().is_none() {
            ManagementCommand::DiskDetach { id }
        } else {
            ManagementCommand::Invalid
        };
    }
    ManagementCommand::Invalid
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
                attach: false,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b" VM START -a "),
            ManagementCommand::VmStart {
                id: 0,
                attach: true,
                disk: None,
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
                memory_mib: 128,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b"vm create 1G"),
            ManagementCommand::VmCreate {
                id: None,
                memory_mib: 1024,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b"vm create 4096 128M"),
            ManagementCommand::VmCreate {
                id: Some(4096),
                memory_mib: 128,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b"vm start 3 --attach"),
            ManagementCommand::VmStart {
                id: 3,
                attach: true,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b"vm start -a 2"),
            ManagementCommand::VmStart {
                id: 2,
                attach: true,
                disk: None,
            }
        );
        assert_eq!(
            parse_command(b"vm stop 1"),
            ManagementCommand::VmStop { id: 1 }
        );
        assert_eq!(
            parse_command(b"vm delete 1"),
            ManagementCommand::VmDelete { id: 1 }
        );
        assert_eq!(
            parse_command(b"remove 2"),
            ManagementCommand::VmDelete { id: 2 }
        );
        assert_eq!(
            parse_command(b"serial attach 2"),
            ManagementCommand::SerialAttach { id: 2 }
        );
        assert_eq!(parse_command(b"vm list"), ManagementCommand::VmList);
    }

    #[test]
    fn parses_disk_selection_commands() {
        assert_eq!(
            parse_command(b"vm create 2 512M --disk 1"),
            ManagementCommand::VmCreate {
                id: Some(2),
                memory_mib: 512,
                disk: Some(1),
            }
        );
        assert_eq!(
            parse_command(b"vm start 2 --disk 1 -a"),
            ManagementCommand::VmStart {
                id: 2,
                attach: true,
                disk: Some(1),
            }
        );
        assert_eq!(parse_command(b"disk list"), ManagementCommand::DiskList);
        assert_eq!(parse_command(b"vm disk list"), ManagementCommand::DiskList);
        assert_eq!(
            parse_command(b"vm disk attach 2 1"),
            ManagementCommand::DiskAttach { id: 2, disk: 1 }
        );
        assert_eq!(
            parse_command(b"disk detach 2"),
            ManagementCommand::DiskDetach { id: 2 }
        );
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
