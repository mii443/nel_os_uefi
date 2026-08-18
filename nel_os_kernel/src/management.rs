pub(crate) const BANNER: &[u8] =
    b"nel hypervisor management shell\r\nType 'help' for commands.\r\nnel> ";
pub(crate) const PROMPT: &[u8] = b"nel> ";
pub(crate) const HELP: &[u8] = b"Commands:\r\n  vm start [--attach|-a]\r\n  vm stop|reset|status\r\n  serial attach|detach\r\n  info memory|runtime|vm|all\r\n  help\r\n  exit\r\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementCommand {
    VmStart,
    VmStartAttach,
    VmStop,
    VmReset,
    VmStatus,
    SerialAttach,
    SerialDetach,
    InfoMemory,
    InfoRuntime,
    InfoAll,
    Help,
    Prompt,
    Invalid,
    Disconnect,
}

pub(crate) fn parse_command(line: &[u8]) -> ManagementCommand {
    let line = trim_ascii(line);
    if line.eq_ignore_ascii_case(b"vm start") || line.eq_ignore_ascii_case(b"start") {
        ManagementCommand::VmStart
    } else if line.eq_ignore_ascii_case(b"vm start --attach")
        || line.eq_ignore_ascii_case(b"vm start -a")
        || line.eq_ignore_ascii_case(b"start --attach")
        || line.eq_ignore_ascii_case(b"start -a")
    {
        ManagementCommand::VmStartAttach
    } else if line.eq_ignore_ascii_case(b"vm stop") || line.eq_ignore_ascii_case(b"stop") {
        ManagementCommand::VmStop
    } else if line.eq_ignore_ascii_case(b"vm reset") || line.eq_ignore_ascii_case(b"reset") {
        ManagementCommand::VmReset
    } else if line.eq_ignore_ascii_case(b"vm status")
        || line.eq_ignore_ascii_case(b"vm info")
        || line.eq_ignore_ascii_case(b"info vm")
    {
        ManagementCommand::VmStatus
    } else if line.eq_ignore_ascii_case(b"serial attach") {
        ManagementCommand::SerialAttach
    } else if line.eq_ignore_ascii_case(b"serial detach") {
        ManagementCommand::SerialDetach
    } else if line.eq_ignore_ascii_case(b"info memory") {
        ManagementCommand::InfoMemory
    } else if line.eq_ignore_ascii_case(b"info runtime") {
        ManagementCommand::InfoRuntime
    } else if line.eq_ignore_ascii_case(b"info all") || line.eq_ignore_ascii_case(b"info") {
        ManagementCommand::InfoAll
    } else if line.eq_ignore_ascii_case(b"help") || line == b"?" {
        ManagementCommand::Help
    } else if line.eq_ignore_ascii_case(b"exit") || line.eq_ignore_ascii_case(b"quit") {
        ManagementCommand::Disconnect
    } else {
        ManagementCommand::Invalid
    }
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
    fn parses_aliases_and_attach_option() {
        assert_eq!(parse_command(b"start"), ManagementCommand::VmStart);
        assert_eq!(
            parse_command(b" VM START -a "),
            ManagementCommand::VmStartAttach
        );
        assert_eq!(parse_command(b"vm info"), ManagementCommand::VmStatus);
        assert_eq!(parse_command(b"info vm"), ManagementCommand::VmStatus);
        assert_eq!(parse_command(b"?"), ManagementCommand::Help);
    }
}
