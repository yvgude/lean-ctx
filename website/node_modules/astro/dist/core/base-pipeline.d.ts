import type { $ZodType } from 'zod/v4/core';
import type { ActionAccept, ActionClient } from '../actions/runtime/types.js';
import type { ComponentInstance } from '../types/astro.js';
import type { MiddlewareHandler, RewritePayload } from '../types/public/common.js';
import type { RuntimeMode } from '../types/public/config.js';
import type { RouteData, SSRActions, SSRLoadedRenderer, SSRManifest, SSRResult } from '../types/public/internal.js';
import type { ServerIslandMappings } from './app/types.js';
import type { SinglePageBuiltModule } from './build/types.js';
import type { Logger } from './logger/core.js';
import { RouteCache } from './render/route-cache.js';
import type { CacheProvider, CacheProviderFactory } from './cache/types.js';
import type { CompiledCacheRoute } from './cache/runtime/route-matching.js';
import type { SessionDriverFactory } from './session/types.js';
import { NodePool } from '../runtime/server/render/queue/pool.js';
import { HTMLStringCache } from '../runtime/server/html-string-cache.js';
/**
 * The `Pipeline` represents the static parts of rendering that do not change between requests.
 * These are mostly known when the server first starts up and do not change.
 *
 * Thus, a `Pipeline` is created once at process start and then used by every `RenderContext`.
 */
export declare abstract class Pipeline {
    readonly logger: Logger;
    readonly manifest: SSRManifest;
    /**
     * "development" or "production" only
     */
    readonly runtimeMode: RuntimeMode;
    readonly renderers: SSRLoadedRenderer[];
    readonly resolve: (s: string) => Promise<string>;
    readonly streaming: boolean;
    /**
     * Used to provide better error messages for `Astro.clientAddress`
     */
    readonly adapterName: string;
    readonly clientDirectives: Map<string, string>;
    readonly inlinedScripts: Map<string, string>;
    readonly compressHTML: boolean;
    readonly i18n: import("./app/types.js").SSRManifestI18n | undefined;
    readonly middleware: (() => Promise<import("../types/public/common.js").AstroMiddlewareInstance> | import("../types/public/common.js").AstroMiddlewareInstance) | undefined;
    readonly routeCache: RouteCache;
    /**
     * Used for `Astro.site`.
     */
    readonly site: URL | undefined;
    /**
     * Array of built-in, internal, routes.
     * Used to find the route module
     */
    readonly defaultRoutes: {
        instance: ComponentInstance;
        matchesComponent(filePath: URL): boolean;
        route: string;
        component: string;
    }[];
    readonly actions: (() => Promise<SSRActions> | SSRActions) | undefined;
    readonly sessionDriver: (() => Promise<{
        default: SessionDriverFactory | null;
    }>) | undefined;
    readonly cacheProvider: (() => Promise<{
        default: CacheProviderFactory | null;
    }>) | undefined;
    readonly cacheConfig: import("./cache/types.js").SSRManifestCache | undefined;
    readonly serverIslands: (() => Promise<ServerIslandMappings> | ServerIslandMappings) | undefined;
    readonly internalMiddleware: MiddlewareHandler[];
    resolvedMiddleware: MiddlewareHandler | undefined;
    resolvedActions: SSRActions | undefined;
    resolvedSessionDriver: SessionDriverFactory | null | undefined;
    resolvedCacheProvider: CacheProvider | null | undefined;
    compiledCacheRoutes: CompiledCacheRoute[] | undefined;
    nodePool: NodePool | undefined;
    htmlStringCache: HTMLStringCache | undefined;
    constructor(logger: Logger, manifest: SSRManifest, 
    /**
     * "development" or "production" only
     */
    runtimeMode: RuntimeMode, renderers: SSRLoadedRenderer[], resolve: (s: string) => Promise<string>, streaming: boolean, 
    /**
     * Used to provide better error messages for `Astro.clientAddress`
     */
    adapterName?: string, clientDirectives?: Map<string, string>, inlinedScripts?: Map<string, string>, compressHTML?: boolean, i18n?: import("./app/types.js").SSRManifestI18n | undefined, middleware?: (() => Promise<import("../types/public/common.js").AstroMiddlewareInstance> | import("../types/public/common.js").AstroMiddlewareInstance) | undefined, routeCache?: RouteCache, 
    /**
     * Used for `Astro.site`.
     */
    site?: URL | undefined, 
    /**
     * Array of built-in, internal, routes.
     * Used to find the route module
     */
    defaultRoutes?: {
        instance: ComponentInstance;
        matchesComponent(filePath: URL): boolean;
        route: string;
        component: string;
    }[], actions?: (() => Promise<SSRActions> | SSRActions) | undefined, sessionDriver?: (() => Promise<{
        default: SessionDriverFactory | null;
    }>) | undefined, cacheProvider?: (() => Promise<{
        default: CacheProviderFactory | null;
    }>) | undefined, cacheConfig?: import("./cache/types.js").SSRManifestCache | undefined, serverIslands?: (() => Promise<ServerIslandMappings> | ServerIslandMappings) | undefined);
    abstract headElements(routeData: RouteData): Promise<HeadElements> | HeadElements;
    abstract componentMetadata(routeData: RouteData): Promise<SSRResult['componentMetadata']> | void;
    /**
     * It attempts to retrieve the `RouteData` that matches the input `url`, and the component that belongs to the `RouteData`.
     *
     * ## Errors
     *
     * - if not `RouteData` is found
     *
     * @param {RewritePayload} rewritePayload The payload provided by the user
     * @param {Request} request The original request
     */
    abstract tryRewrite(rewritePayload: RewritePayload, request: Request): Promise<TryRewriteResult>;
    /**
     * Tells the pipeline how to retrieve a component give a `RouteData`
     * @param routeData
     */
    abstract getComponentByRoute(routeData: RouteData): Promise<ComponentInstance>;
    /**
     * The current name of the pipeline. Useful for debugging
     */
    abstract getName(): string;
    /**
     * Resolves the middleware from the manifest, and returns the `onRequest` function. If `onRequest` isn't there,
     * it returns a no-op function
     */
    getMiddleware(): Promise<MiddlewareHandler>;
    getActions(): Promise<SSRActions>;
    getSessionDriver(): Promise<SessionDriverFactory | null>;
    getCacheProvider(): Promise<CacheProvider | null>;
    getServerIslands(): Promise<ServerIslandMappings>;
    getAction(path: string): Promise<ActionClient<unknown, ActionAccept, $ZodType>>;
    getModuleForRoute(route: RouteData): Promise<SinglePageBuiltModule>;
    createNodePool(poolSize: number, stats: boolean): NodePool;
    createStringCache(): HTMLStringCache;
}
export interface HeadElements extends Pick<SSRResult, 'scripts' | 'styles' | 'links'> {
}
export interface TryRewriteResult {
    routeData: RouteData;
    componentInstance: ComponentInstance;
    newUrl: URL;
    pathname: string;
}
