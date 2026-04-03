import type http from 'node:http';
import { BaseApp, type RenderErrorOptions } from '../core/app/entrypoints/index.js';
import type { Logger } from '../core/logger/core.js';
import type { ModuleLoader } from '../core/module-loader/index.js';
import type { CreateRenderContext, RenderContext } from '../core/render-context.js';
import type { AstroSettings, RoutesList } from '../types/astro.js';
import type { RouteData, SSRManifest } from '../types/public/index.js';
import type { DevServerController } from '../vite-plugin-astro-server/controller.js';
import { RunnablePipeline } from './pipeline.js';
import type { DevMatch, LogRequestPayload } from '../core/app/base.js';
export declare class AstroServerApp extends BaseApp<RunnablePipeline> {
    settings: AstroSettings;
    logger: Logger;
    loader: ModuleLoader;
    manifestData: RoutesList;
    currentRenderContext: RenderContext | undefined;
    constructor(manifest: SSRManifest, streaming: boolean | undefined, logger: Logger, manifestData: RoutesList, loader: ModuleLoader, settings: AstroSettings, getDebugInfo: () => Promise<string>);
    isDev(): boolean;
    /**
     * Updates the routes list when files change during development.
     * Called via HMR when new pages are added/removed.
     */
    updateRoutes(newRoutesList: RoutesList): void;
    /**
     * Clears the route cache so that getStaticPaths() is re-evaluated.
     * Called via HMR when content collection data changes.
     */
    clearRouteCache(): void;
    devMatch(pathname: string): Promise<DevMatch | undefined>;
    static create(manifest: SSRManifest, routesList: RoutesList, logger: Logger, loader: ModuleLoader, settings: AstroSettings, getDebugInfo: () => Promise<string>): Promise<AstroServerApp>;
    createPipeline(_streaming: boolean, manifest: SSRManifest, settings: AstroSettings, logger: Logger, loader: ModuleLoader, manifestData: RoutesList, getDebugInfo: () => Promise<string>): RunnablePipeline;
    createRenderContext(payload: CreateRenderContext): Promise<RenderContext>;
    handleRequest({ controller, incomingRequest, incomingResponse, isHttps, }: HandleRequest): Promise<void>;
    match(request: Request, _allowPrerenderedRoutes: boolean): RouteData | undefined;
    renderError(request: Request, { skipMiddleware, error, status, response: _response, ...resolvedRenderOptions }: RenderErrorOptions): Promise<Response>;
    logRequest({ pathname, method, statusCode, isRewrite, reqTime }: LogRequestPayload): void;
}
type HandleRequest = {
    controller: DevServerController;
    incomingRequest: http.IncomingMessage;
    incomingResponse: http.ServerResponse;
    isHttps: boolean;
};
export {};
