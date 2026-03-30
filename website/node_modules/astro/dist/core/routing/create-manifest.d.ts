import nodeFs from 'node:fs';
import type { AstroSettings, RoutesList } from '../../types/astro.js';
import type { RouteData } from '../../types/public/internal.js';
import type { Logger } from '../logger/core.js';
export interface RouteEntry {
    path: string;
    isDir: boolean;
}
type RoutingSettings = Pick<AstroSettings, 'config' | 'injectedRoutes' | 'pageExtensions' | 'buildOutput'>;
interface CreateRouteManifestParams {
    /** Astro Settings object */
    settings: AstroSettings;
    /** Current working directory */
    cwd?: string;
    /** fs module, for testing */
    fsMod?: typeof nodeFs;
}
export declare function createRoutesFromEntries(entries: RouteEntry[], settings: RoutingSettings, logger: Logger, pagesDirRelative?: string): RouteData[];
/**
 * Create a full route manifest from filesystem and injected routes.
 */
export declare function createRoutesList(params: CreateRouteManifestParams, logger: Logger, { dev, }?: {
    dev?: boolean;
}): Promise<RoutesList>;
/**
 * Resolve a route entrypoint to an absolute component path.
 */
export declare function resolveInjectedRoute(entrypoint: string, root: URL, cwd?: string): {
    resolved: string;
    component: string;
};
export {};
