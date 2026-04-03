import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { XMLLintUnavailable } from './errors.js';
/**
 * Finds the `schema` directory with robust path resolution.
 * Searches from the project root directory using process.cwd().
 * This works correctly regardless of whether the code is running from:
 * - Source: lib/xmllint.ts
 * - ESM build: dist/esm/lib/xmllint.js
 * - CJS build: dist/cjs/lib/xmllint.js
 * - Test environment
 *
 * @throws {Error} if the schema directory is not found
 * @returns {string} the path to the schema directory
 */
function findSchemaDir() {
    // Search for schema directory from project root
    // This works in test, build, and source environments
    const possiblePaths = [
        resolve(process.cwd(), 'schema'), // From project root
        resolve(process.cwd(), '..', 'schema'), // One level up
        resolve(process.cwd(), '..', '..', 'schema'), // Two levels up
    ];
    for (const schemaPath of possiblePaths) {
        if (existsSync(schemaPath)) {
            return schemaPath;
        }
    }
    throw new Error(`Schema directory not found. Searched paths: ${possiblePaths.join(', ')}`);
}
/**
 * Verify the passed in xml is valid. Requires xmllib be installed
 *
 * Security: This function always pipes XML content via stdin to prevent
 * command injection vulnerabilities. Never pass user-controlled strings
 * as file path arguments to xmllint.
 *
 * @param xml what you want validated (string or Readable stream)
 * @return {Promise<void>} resolves on valid rejects [error stderr]
 */
export function xmlLint(xml) {
    const args = [
        '--schema',
        resolve(findSchemaDir(), 'all.xsd'),
        '--noout',
        '-', // Always read from stdin for security
    ];
    return new Promise((resolve, reject) => {
        execFile('which', ['xmllint'], (error, stdout, stderr) => {
            if (error) {
                reject([new XMLLintUnavailable()]);
                return;
            }
            const xmllint = execFile('xmllint', args, (error, stdout, stderr) => {
                if (error) {
                    reject([error, stderr]);
                }
                resolve();
            });
            // Always pipe XML content via stdin for security
            if (xmllint.stdin) {
                if (typeof xml === 'string') {
                    // Convert string to stream and pipe to stdin
                    xmllint.stdin.write(xml);
                    xmllint.stdin.end();
                }
                else if (xml) {
                    // Pipe readable stream to stdin
                    xml.pipe(xmllint.stdin);
                }
            }
            if (xmllint.stdout) {
                xmllint.stdout.unpipe();
            }
        });
    });
}
