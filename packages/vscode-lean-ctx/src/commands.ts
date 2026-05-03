import * as vscode from "vscode";
import { runCommand, getVersion } from "./binary";

let outputChannel: vscode.OutputChannel | null = null;

function getOutputChannel(): vscode.OutputChannel {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel("lean-ctx");
  }
  return outputChannel;
}

async function runInOutputChannel(
  title: string,
  args: string[]
): Promise<void> {
  const channel = getOutputChannel();
  channel.show(true);
  channel.appendLine(`\n━━━ ${title} ━━━`);
  channel.appendLine(`> lean-ctx ${args.join(" ")}\n`);

  try {
    const { stdout, stderr } = await runCommand(args);
    if (stdout) {
      channel.appendLine(stdout);
    }
    if (stderr) {
      channel.appendLine(stderr);
    }
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    channel.appendLine(`Error: ${message}`);
    vscode.window.showErrorMessage(`lean-ctx: ${message}`);
  }
}

export async function cmdSetup(): Promise<void> {
  await runInOutputChannel("Setup", ["setup"]);
}

export async function cmdDoctor(): Promise<void> {
  await runInOutputChannel("Doctor", ["doctor"]);
}

export async function cmdGain(): Promise<void> {
  await runInOutputChannel("Token Savings", ["gain"]);
}

export async function cmdDashboard(): Promise<void> {
  const channel = getOutputChannel();
  channel.show(true);
  channel.appendLine("\n━━━ Dashboard ━━━");

  try {
    const { stdout } = await runCommand(["dashboard", "--port=0"]);
    const portMatch = stdout.match(/localhost:(\d+)/);
    if (portMatch) {
      const url = `http://localhost:${portMatch[1]}`;
      vscode.env.openExternal(vscode.Uri.parse(url));
      channel.appendLine(`Dashboard opened: ${url}`);
    } else {
      channel.appendLine(stdout);
    }
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    channel.appendLine(`Error: ${message}`);
    channel.appendLine(
      "Tip: Run 'lean-ctx dashboard' in terminal for the web dashboard."
    );
  }
}

export async function cmdHeatmap(): Promise<void> {
  await runInOutputChannel("Context Heatmap", ["heatmap"]);
}

export async function showWelcome(): Promise<void> {
  const version = await getVersion();
  if (version) {
    const channel = getOutputChannel();
    channel.appendLine(
      `lean-ctx v${version} activated — 49 MCP tools, 10 read modes, 90+ compression patterns`
    );
  }
}

export function disposeOutputChannel(): void {
  outputChannel?.dispose();
}
