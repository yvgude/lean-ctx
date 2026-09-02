# Installing Lean-CTX on Windows 10 for OpenCode

> **Status: local setup note.** Confirm commands against the installed Runtime;
> this is not a hosted-service or general agent-platform setup guide. LeanCTX is
> **The Context SDK for AI Agents**. Current scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository).
### A Guide for Bridging WSL Ubuntu and OpenCode

This guide outlines the process of installing **Lean-CTX** within a Windows Subsystem for Linux (WSL) environment and integrating it as an MCP server for **OpenCode** on Windows.

---

## Phase 1: WSL Environment Setup

Because native Windows installation can be complex, we utilize **WSL Ubuntu** as the host environment for the Lean-ctx binary.

1. **Install WSL Ubuntu** via the Microsoft Store or PowerShell: `wsl --install -d Ubuntu`.
2. **Install Lean-ctx** by running the following command in your Ubuntu terminal:
   ```bash
   curl -fsSL https://leanctx.com/install.sh | sh
   ```
3. **Configure the PATH** to ensure the binary is globally accessible:
   ```bash
   echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
   source ~/.bashrc
   ```
4. **Verify Installation**:
   ```bash
   lean-ctx --version
   # Expected output: lean-ctx 3.4.7 (official, https://github.com)
   ```

---

## Phase 2: Lean-ctx Configuration

Initialize the setup by running:
```bash
lean-ctx setup
```

Follow the interactive prompts to configure your environment. Note that while the tool sits in Ubuntu, it will effectively manage your Windows-based OpenCode context.

### Recommended Settings


| Feature | Setting | Description |
| :--- | :--- | :--- |
| **Agent Output Optimization** | `lite` or `full` | Reduces "fluff" and increases token density. |
| **Tool Result Archive** | `Enabled` | Archives large outputs to a "filing cabinet" to save context space. |
| **Output Density** | `Terse` | Removes terminal noise and redundant headers. |

---

## Phase 3: Diagnostics and Dashboard

1. **Verify Health**:
   ```bash
   lean-ctx doctor
   # Ensure output ends with: Summary: 11/11 checks passed
   ```
2. **Initialize Agent Profile**:
   ```bash
   lean-ctx init --agent opencode
   ```
3. **Launch Dashboard**:
   Keep your Ubuntu terminal open and run:
   ```bash
   lean-ctx dashboard
   ```
   You can now access the visual performance monitor at: `http://127.0.0.1:3333`

---

## Phase 4: OpenCode MCP Integration (Windows)

Now, return to your **Windows Command Prompt** to bridge OpenCode to your WSL environment.

1. **Add the MCP Server**:
   ```powershell
   opencode-cli mcp add
   ```
   **Interactive Input:**
   - **Name**: `lean-ctx`
   - **Type**: `local`
   - **Command**: `wsl.exe` (This acts as a placeholder for the next step).

2. **Gather Environment Metadata**:
   Run these commands to get the required values for your config:
   - **Distro Name**: `wsl -l -v` (Usually `Ubuntu`)
   - **WSL Username**: `wsl whoami` (Note: This is your Linux username, usually lowercase).

3. **Edit `opencode.json`**:
   Navigate to `%USERPROFILE%\.config\opencode\opencode.json` and locate the `lean-ctx` entry. Replace the `"command": "wsl.exe"` string with a structured array:

   ```json
   "mcp": {
     "lean-ctx": {
       "type": "local",
       "command": [
         "wsl.exe", 
         "-d", "Ubuntu", 
         "-e", "/home/<Your_WSL_Username>/.local/bin/lean-ctx"
       ],
       "enabled": true
     }
   }
   ```

---

## Phase 5: Verification

1. **Launch OpenCode**: The GUI should indicate that the MCP server is connected and enabled.
2. **Test Tool Usage**: Give your agent a high-load command to force an archive trigger:
   > *"Aggressively use ctx_archive for any file read exceeding 300 lines to maintain maximum context overhead."*
3. **Monitor Performance**: Check the dashboard at `http://127.0.0.1:3333` to see real-time tool calls and compression ratios.

---
*Special thanks to **Yves Gugger** for developing Lean-CTX.*
