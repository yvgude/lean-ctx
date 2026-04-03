import type * as vite from 'vite';
import type { SSRManifest } from '../core/app/types.js';
import type { Logger } from '../core/logger/core.js';
import type { AstroSettings } from '../types/astro.js';
interface AstroPluginOptions {
    settings: AstroSettings;
    logger: Logger;
}
export default function createVitePluginAstroServer({ settings, logger, }: AstroPluginOptions): vite.Plugin;
/**
 * It creates a `SSRManifest` from the `AstroSettings`.
 *
 * Renderers needs to be pulled out from the page module emitted during the build.
 * @param settings
 */
export declare function createDevelopmentManifest(settings: AstroSettings): Promise<SSRManifest>;
export {};
