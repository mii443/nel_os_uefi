use core::fmt;

use crate::{
    management::{BANNER, HELP, ManagementCommand, PROMPT, parse_command},
    serial,
};

const COMMAND_QUEUE_LEN: usize = 8;
const INPUT_QUEUE_LEN: usize = 512;

pub(crate) struct SerialConsole {
    attached: bool,
    guest_input_overflowed: bool,
    line: [u8; 256],
    line_len: usize,
    last_input_was_cr: bool,
    pending_input: [u8; INPUT_QUEUE_LEN],
    pending_head: usize,
    pending_len: usize,
    commands: [Option<ManagementCommand>; COMMAND_QUEUE_LEN],
    command_head: usize,
    command_len: usize,
}

impl SerialConsole {
    pub(crate) const fn new() -> Self {
        Self {
            attached: false,
            guest_input_overflowed: false,
            line: [0; 256],
            line_len: 0,
            last_input_was_cr: false,
            pending_input: [0; INPUT_QUEUE_LEN],
            pending_head: 0,
            pending_len: 0,
            commands: [None; COMMAND_QUEUE_LEN],
            command_head: 0,
            command_len: 0,
        }
    }

    pub(crate) fn activate(&mut self) {
        self.set_serial_attached(false);
        self.write_bytes(BANNER);
    }

    pub(crate) fn poll(&mut self) {
        // Drain the hardware FIFO before executing a potentially slow command
        // such as first-time guest RAM allocation. Bytes after the completed
        // command remain tagged by their position in this software queue and
        // are interpreted only after the controller applies the new mode.
        while self.pending_len < self.pending_input.len() {
            let mut input = [0u8; 1];
            if serial::poll_input(&mut input) == 0 {
                break;
            }
            let tail = (self.pending_head + self.pending_len) % self.pending_input.len();
            self.pending_input[tail] = input[0];
            self.pending_len += 1;
        }

        for _ in 0..256 {
            let Some(byte) = self.pop_pending_input() else {
                break;
            };

            // Consume the LF half of a CRLF pair even when the CR command
            // enabled serial attachment.
            if self.last_input_was_cr {
                self.last_input_was_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }

            if self.attached {
                if byte == 0x1d {
                    let guest_input_overflowed = self.guest_input_overflowed;
                    self.attached = false;
                    self.guest_input_overflowed = false;
                    serial::set_local_guest_output_enabled(false);
                    self.last_input_was_cr = true;
                    self.write_bytes(b"\r\nDetached from VM serial.\r\n");
                    if guest_input_overflowed {
                        self.write_bytes(b"WARN guest serial input overflowed before detach.\r\n");
                    }
                    self.prompt();
                    break;
                } else if serial::queue_guest_input(&[byte]) != 1 {
                    // COM1 has no protocol-level flow control. Keep polling so
                    // Ctrl-] can always reclaim the management console.
                    self.guest_input_overflowed = true;
                }
            } else if self.consume_management_byte(byte) {
                break;
            }
        }
    }

    pub(crate) fn take_command(&mut self) -> Option<ManagementCommand> {
        if self.command_len == 0 {
            return None;
        }
        let command = self.commands[self.command_head].take();
        self.command_head = (self.command_head + 1) % self.commands.len();
        self.command_len -= 1;
        command
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        serial::write_bytes(bytes);
    }

    pub(crate) fn prompt(&mut self) {
        self.write_bytes(PROMPT);
    }

    pub(crate) fn write_help(&mut self) {
        self.write_bytes(HELP);
    }

    pub(crate) fn set_serial_attached(&mut self, attached: bool) {
        self.attached = attached;
        if !attached {
            self.guest_input_overflowed = false;
            self.pending_head = 0;
            self.pending_len = 0;
            self.line_len = 0;
            self.last_input_was_cr = false;
        }
        serial::set_local_guest_output_enabled(attached);
    }

    pub(crate) fn serial_attached(&self) -> bool {
        self.attached
    }

    fn pop_pending_input(&mut self) -> Option<u8> {
        if self.pending_len == 0 {
            return None;
        }
        let byte = self.pending_input[self.pending_head];
        self.pending_head = (self.pending_head + 1) % self.pending_input.len();
        self.pending_len -= 1;
        Some(byte)
    }

    /// Returns true when command execution may change the input mode.
    fn consume_management_byte(&mut self, byte: u8) -> bool {
        match byte {
            b'\r' => {
                self.write_bytes(b"\r\n");
                self.finish_or_prompt();
                self.last_input_was_cr = true;
                true
            }
            b'\n' => {
                self.write_bytes(b"\r\n");
                self.finish_or_prompt();
                true
            }
            0x08 | 0x7f => {
                if self.line_len != 0 {
                    self.line_len -= 1;
                    self.write_bytes(b"\x08 \x08");
                }
                false
            }
            byte if self.line_len < self.line.len() => {
                self.line[self.line_len] = byte;
                self.line_len += 1;
                self.write_bytes(&[byte]);
                false
            }
            _ => {
                self.line_len = 0;
                self.write_bytes(b"\r\nERR command is too long\r\n");
                self.prompt();
                true
            }
        }
    }

    fn finish_or_prompt(&mut self) {
        if self.line_len == 0 {
            self.enqueue_command(ManagementCommand::Prompt);
            return;
        }

        let command = parse_command(&self.line[..self.line_len]);
        self.line_len = 0;
        self.enqueue_command(command);
    }

    fn enqueue_command(&mut self, command: ManagementCommand) {
        if self.command_len == self.commands.len() {
            self.write_bytes(b"ERR management queue is busy\r\n");
            self.prompt();
            return;
        }
        let tail = (self.command_head + self.command_len) % self.commands.len();
        self.commands[tail] = Some(command);
        self.command_len += 1;
    }
}

impl fmt::Write for SerialConsole {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_bytes(text.as_bytes());
        Ok(())
    }
}
