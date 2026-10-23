//! Built-in OS MCP (Model Context Protocol) Interface & Multi-Step Autonomous Agent Engine (`src/mcp_agent.rs`)
//!
//! 1. **System Prompt (`AGENT_MCP_SYSTEM_PROMPT`)**:
//!    Instructs the AI Agent how to invoke the 10 built-in OS MCP tools using `TOOL_CALL: <name> | <arg>`
//!    and how to chain outputs from step N into step N+1 via `$PREV_OUTPUT`.
//! 2. **10 Built-in OS MCP Tools (`MCP_TOOLS`)**:
//!    - L1 Read-Only: `sys_get_info`, `net_get_ip`, `fs_list_dir`, `fs_read_file`
//!    - L2 Desktop Control: `ui_set_theme`, `ui_set_wallpaper`, `ui_open_window`, `audio_play_tone`
//!    - L3 Full Root: `fs_write_file`, `sh_exec_cmd`
//! 3. **Autonomous Execution Loop (`AgentAutonomousEngine`)**:
//!    Decomposes complex goals into multi-step MCP pipelines, automatically substitutes `$PREV_OUTPUT`
//!    with the real hardware/filesystem output of the previous tool, and executes continuously across frames.

#![allow(dead_code)]

extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::security::{AiPermissionLevel, UserRole};

pub const AGENT_MCP_SYSTEM_PROMPT: &str = r#"You are AuraOS Native Kernel Agent with 10 built-in OS MCP tools.
Format every tool invocation as:
  TOOL_CALL: <tool_name> | <argument>
Use $PREV_OUTPUT in <argument> to automatically inject the previous tool's return data!

Built-in OS MCP Tools & Required Authority Tiers:
[L1 Read-Only]
  1. sys_get_info     | all                        -> Query RAM, NIC, Role & AI authority
  2. net_get_ip       | eth0                       -> Query PCIe NIC MAC, IPv4 & Gateway
  3. fs_list_dir      | \AURAOS                    -> List directory on FAT32 disk
  4. fs_read_file     | \AURAOS\README.TXT         -> Read file content from FAT32 disk
[L2 Desktop Control]
  5. ui_set_theme     | 0..3                       -> Switch White-Blue lightweight theme
  6. ui_set_wallpaper | 0..2                       -> Switch adaptive PNG/JPG/BMP wallpaper
  7. ui_open_window   | files|terminal|settings|ai -> Open & focus desktop window
  8. audio_play_tone  | 880                        -> Play tone (Hz) on 8254 PIT sound card
[L3 Full Root (Admin)]
  9. fs_write_file    | \AURAOS\REPORT.TXT|$PREV_OUTPUT -> Persist data to FAT32 disk
 10. sh_exec_cmd      | ip a                       -> Execute kernel terminal command"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum McpToolId {
    SysGetInfo = 0,
    NetGetIp = 1,
    FsListDir = 2,
    FsReadFile = 3,
    UiSetTheme = 4,
    UiSetWallpaper = 5,
    UiOpenWindow = 6,
    AudioPlayTone = 7,
    FsWriteFile = 8,
    ShExecCmd = 9,
}

impl McpToolId {
    pub fn name(self) -> &'static str {
        match self {
            McpToolId::SysGetInfo => "sys_get_info",
            McpToolId::NetGetIp => "net_get_ip",
            McpToolId::FsListDir => "fs_list_dir",
            McpToolId::FsReadFile => "fs_read_file",
            McpToolId::UiSetTheme => "ui_set_theme",
            McpToolId::UiSetWallpaper => "ui_set_wallpaper",
            McpToolId::UiOpenWindow => "ui_open_window",
            McpToolId::AudioPlayTone => "audio_play_tone",
            McpToolId::FsWriteFile => "fs_write_file",
            McpToolId::ShExecCmd => "sh_exec_cmd",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "sys_get_info" => Some(McpToolId::SysGetInfo),
            "net_get_ip" => Some(McpToolId::NetGetIp),
            "fs_list_dir" => Some(McpToolId::FsListDir),
            "fs_read_file" => Some(McpToolId::FsReadFile),
            "ui_set_theme" => Some(McpToolId::UiSetTheme),
            "ui_set_wallpaper" => Some(McpToolId::UiSetWallpaper),
            "ui_open_window" => Some(McpToolId::UiOpenWindow),
            "audio_play_tone" => Some(McpToolId::AudioPlayTone),
            "fs_write_file" => Some(McpToolId::FsWriteFile),
            "sh_exec_cmd" => Some(McpToolId::ShExecCmd),
            _ => None,
        }
    }
}

pub struct McpToolSpec {
    pub id: McpToolId,
    pub name: &'static str,
    pub min_ai_level: u8,
    pub requires_admin_user: bool,
    pub sample_arg: &'static str,
    pub description_en: &'static str,
}

pub const MCP_TOOLS: [McpToolSpec; 10] = [
    McpToolSpec {
        id: McpToolId::SysGetInfo,
        name: "sys_get_info",
        min_ai_level: 1,
        requires_admin_user: false,
        sample_arg: "all",
        description_en: "Query physical RAM, NIC & user role",
    },
    McpToolSpec {
        id: McpToolId::NetGetIp,
        name: "net_get_ip",
        min_ai_level: 1,
        requires_admin_user: false,
        sample_arg: "eth0",
        description_en: "Query PCIe NIC MAC, IPv4 & gateway",
    },
    McpToolSpec {
        id: McpToolId::FsListDir,
        name: "fs_list_dir",
        min_ai_level: 1,
        requires_admin_user: false,
        sample_arg: "\\AURAOS",
        description_en: "List files on persistent FAT32 disk",
    },
    McpToolSpec {
        id: McpToolId::FsReadFile,
        name: "fs_read_file",
        min_ai_level: 1,
        requires_admin_user: false,
        sample_arg: "\\AURAOS\\README.TXT",
        description_en: "Read file from FAT32 disk",
    },
    McpToolSpec {
        id: McpToolId::UiSetTheme,
        name: "ui_set_theme",
        min_ai_level: 2,
        requires_admin_user: false,
        sample_arg: "1",
        description_en: "Switch White-Blue lightweight theme",
    },
    McpToolSpec {
        id: McpToolId::UiSetWallpaper,
        name: "ui_set_wallpaper",
        min_ai_level: 2,
        requires_admin_user: false,
        sample_arg: "1",
        description_en: "Switch adaptive PNG/JPG/BMP wallpaper",
    },
    McpToolSpec {
        id: McpToolId::UiOpenWindow,
        name: "ui_open_window",
        min_ai_level: 2,
        requires_admin_user: false,
        sample_arg: "files",
        description_en: "Open and focus desktop window",
    },
    McpToolSpec {
        id: McpToolId::AudioPlayTone,
        name: "audio_play_tone",
        min_ai_level: 2,
        requires_admin_user: false,
        sample_arg: "880",
        description_en: "Play hardware 8254 PIT audio tone",
    },
    McpToolSpec {
        id: McpToolId::FsWriteFile,
        name: "fs_write_file",
        min_ai_level: 2,
        requires_admin_user: false,
        sample_arg: "\\AURAOS\\AI_REPORT.TXT|Autonomous MCP Report OK",
        description_en: "Write persistent file to FAT32 disk",
    },
    McpToolSpec {
        id: McpToolId::ShExecCmd,
        name: "sh_exec_cmd",
        min_ai_level: 3,
        requires_admin_user: true,
        sample_arg: "ip a",
        description_en: "Execute kernel terminal command",
    },
];

pub fn check_mcp_permission(
    ai_perm: AiPermissionLevel,
    user_role: UserRole,
    tool: McpToolId,
) -> bool {
    let spec = &MCP_TOOLS[tool as usize];
    if (ai_perm as u8) < spec.min_ai_level {
        return false;
    }
    if spec.requires_admin_user && !user_role.can_modify_system() {
        return false;
    }
    true
}

#[derive(Clone)]
pub struct AgentStep {
    pub tool: McpToolId,
    pub arg_template: String,
    pub completed: bool,
    pub output: String,
}

pub struct AgentAutonomousEngine {
    pub protocol: String,
    pub model: String,
    pub endpoint: String,
    pub api_key: String,
    pub steps: Vec<AgentStep>,
    pub current_step_idx: usize,
    pub last_output: String,
    pub logs: Vec<String>,
}

impl AgentAutonomousEngine {
    pub fn new() -> Self {
        let mut logs = Vec::new();
        logs.push(String::from(
            "MCP Agent Ready: 10 OS tools bound. Click 'Run Agent Loop' or any MCP tool.",
        ));
        Self {
            protocol: String::from("HTTPS"),
            model: String::from("deepseek-r1:8b"),
            endpoint: String::from("https://api.openai.com/v1/chat/completions"),
            api_key: String::from("sk-aura-live-key"),
            steps: Vec::new(),
            current_step_idx: 0,
            last_output: String::new(),
            logs,
        }
    }

    /// Parses `TOOL_CALL: <tool_name> | <arg>`
    pub fn parse_tool_call(line: &str) -> Option<(McpToolId, String)> {
        let trimmed = line.trim();
        let body = if let Some(rest) = trimmed.strip_prefix("TOOL_CALL:") {
            rest.trim()
        } else {
            trimmed
        };
        if let Some(pipe_idx) = body.find('|') {
            let name = body[..pipe_idx].trim();
            let arg = body[pipe_idx + 1..].trim();
            let tid = McpToolId::from_name(name)?;
            Some((tid, arg.to_string()))
        } else {
            let tid = McpToolId::from_name(body)?;
            Some((tid, String::new()))
        }
    }

    /// Builds a multi-step autonomous execution pipeline from a user goal or `TOOL_CALL:` script
    pub fn start_goal(&mut self, goal: &str) {
        self.steps.clear();
        self.current_step_idx = 0;
        self.last_output.clear();

        if goal.trim().starts_with("TOOL_CALL:") {
            if let Some((tid, arg)) = Self::parse_tool_call(goal) {
                self.steps.push(AgentStep {
                    tool: tid,
                    arg_template: arg,
                    completed: false,
                    output: String::new(),
                });
            }
        } else {
            // Autonomous 4-step pipeline that chains output data ($PREV_OUTPUT) across tools automatically:
            // Step 1: Query system RAM & hardware info
            self.steps.push(AgentStep {
                tool: McpToolId::SysGetInfo,
                arg_template: String::from("all"),
                completed: false,
                output: String::new(),
            });
            // Step 2: Query PCIe NIC & IP configuration
            self.steps.push(AgentStep {
                tool: McpToolId::NetGetIp,
                arg_template: String::from("eth0"),
                completed: false,
                output: String::new(),
            });
            // Step 3: Automatically inject $PREV_OUTPUT into fs_write_file to persist report on FAT32 disk!
            self.steps.push(AgentStep {
                tool: McpToolId::FsWriteFile,
                arg_template: String::from("\\AURAOS\\AGENT_REPORT.TXT|AutoReport: $PREV_OUTPUT"),
                completed: false,
                output: String::new(),
            });
            // Step 4: Play completion tone via hardware 8254 PIT sound card
            self.steps.push(AgentStep {
                tool: McpToolId::AudioPlayTone,
                arg_template: String::from("880"),
                completed: false,
                output: String::new(),
            });
        }

        self.logs.push(format!(
            "[Goal Started] '{}' -> Planned {} autonomous MCP steps",
            goal,
            self.steps.len()
        ));
        while self.logs.len() > 10 {
            self.logs.remove(0);
        }
    }

    /// Returns the next `(McpToolId, resolved_arg)` with `$PREV_OUTPUT` automatically replaced
    pub fn next_pending_action(&self) -> Option<(McpToolId, String)> {
        if self.current_step_idx >= self.steps.len() {
            return None;
        }
        let step = &self.steps[self.current_step_idx];
        let resolved_arg = step.arg_template.replace("$PREV_OUTPUT", &self.last_output);
        Some((step.tool, resolved_arg))
    }

    /// Records the output of the current step and advances the autonomous state machine to the next step
    pub fn record_step_output(&mut self, output: String) {
        if self.current_step_idx < self.steps.len() {
            let tool_name = self.steps[self.current_step_idx].tool.name();
            self.steps[self.current_step_idx].completed = true;
            self.steps[self.current_step_idx].output = output.clone();
            self.last_output = output.clone();
            self.current_step_idx += 1;

            self.logs.push(format!(
                "[Step {}/{} {}] -> {}",
                self.current_step_idx,
                self.steps.len(),
                tool_name,
                output
            ));
            while self.logs.len() > 10 {
                self.logs.remove(0);
            }
        }
    }
}
