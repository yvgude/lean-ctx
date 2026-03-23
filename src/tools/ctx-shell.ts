import { exec } from 'node:child_process';
import { resolve } from 'node:path';
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { SessionCache } from '../core/session-cache.js';
import { Compressor } from '../core/compressor.js';
import { getAllPatterns } from '../patterns/index.js';
import { trackSavings } from '../core/token-counter.js';

const MAX_OUTPUT_BYTES = 1024 * 512; // 512KB max raw output
const TIMEOUT_MS = 60_000;

export function registerCtxShell(server: McpServer, cache: SessionCache): void {
  const compressor = new Compressor();
  compressor.registerPatterns(getAllPatterns());

  server.tool(
    'ctx_shell',
    'Execute a shell command and return compressed output. Automatically compresses output from npm, git, docker, tsc, and other common dev tools. Token-efficient alternative to running commands via Shell.',
    {
      command: z.string().describe('Shell command to execute'),
      cwd: z.string().optional().describe('Working directory (default: LEAN_CTX_ROOT or cwd)'),
      timeout: z.number().optional().describe('Timeout in ms (default: 60000)'),
    },
    async ({ command, cwd, timeout = TIMEOUT_MS }) => {
      const workDir = resolve(cwd || process.env.LEAN_CTX_ROOT || process.cwd());

      try {
        const { stdout, stderr } = await execPromise(command, workDir, timeout);
        const combined = [stdout, stderr].filter(Boolean).join('\n');

        if (!combined.trim()) {
          return {
            content: [{ type: 'text' as const, text: `$ ${command}\nok (no output)` }],
          };
        }

        const truncated = combined.length > MAX_OUTPUT_BYTES
          ? combined.slice(0, MAX_OUTPUT_BYTES) + '\n... (output truncated)'
          : combined;

        const result = compressor.compressShellOutput(command, truncated);
        const tokSavings = trackSavings(truncated, result.output);

        return {
          content: [
            { type: 'text' as const, text: `$ ${command}\n${result.output}${tokSavings ? `\n${tokSavings}` : ''}` },
          ],
        };
      } catch (err) {
        if (err instanceof ExecError) {
          const result = compressor.compressShellOutput(command, err.output);
          return {
            content: [
              {
                type: 'text' as const,
                text: `$ ${command}\nExit code: ${err.exitCode}\n${result.output}`,
              },
            ],
            isError: true,
          };
        }

        return {
          content: [
            {
              type: 'text' as const,
              text: `$ ${command}\nError: ${err instanceof Error ? err.message : String(err)}`,
            },
          ],
          isError: true,
        };
      }
    }
  );
}

class ExecError extends Error {
  constructor(
    public exitCode: number,
    public output: string
  ) {
    super(`Command failed with exit code ${exitCode}`);
  }
}

function execPromise(
  command: string,
  cwd: string,
  timeout: number
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    exec(
      command,
      {
        cwd,
        timeout,
        maxBuffer: MAX_OUTPUT_BYTES,
        env: { ...process.env, FORCE_COLOR: '0', NO_COLOR: '1' },
      },
      (error, stdout, stderr) => {
        if (error) {
          const exitCode = error.code
            ? (typeof error.code === 'number' ? error.code : 1)
            : 1;
          const combined = [stdout, stderr].filter(Boolean).join('\n');
          reject(new ExecError(exitCode, combined));
          return;
        }
        resolve({ stdout, stderr });
      }
    );
  });
}
