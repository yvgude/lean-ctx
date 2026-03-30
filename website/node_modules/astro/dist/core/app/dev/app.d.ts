import type { RouteData } from '../../../types/public/index.js';
import type { Logger } from '../../logger/core.js';
import { BaseApp, type DevMatch, type LogRequestPayload, type RenderErrorOptions } from '../base.js';
import type { SSRManifest } from '../types.js';
import { NonRunnablePipeline } from './pipeline.js';
import type { RoutesList } from '../../../types/astro.js';
export declare class DevApp extends BaseApp<NonRunnablePipeline> {
    logger: Logger;
    constructor(manifest: SSRManifest, streaming: boolean | undefined, logger: Logger);
    createPipeline(streaming: boolean, manifest: SSRManifest, logger: Logger): NonRunnablePipeline;
    isDev(): boolean;
    /**
     * Updates the routes list when files change during development.
     * Called via HMR when new pages are added/removed.
     */
    updateRoutes(newRoutesList: RoutesList): void;
    match(request: Request): RouteData | undefined;
    devMatch(pathname: string): Promise<DevMatch | undefined>;
    renderError(request: Request, { skipMiddleware, error, status, response: _response, ...resolvedRenderOptions }: RenderErrorOptions): Promise<Response>;
    logRequest({ pathname, method, statusCode, isRewrite, reqTime }: LogRequestPayload): void;
}
