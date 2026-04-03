import { type HeadElements, Pipeline, type TryRewriteResult } from '../core/base-pipeline.js';
import type { Logger } from '../core/logger/core.js';
import type { ModuleLoader } from '../core/module-loader/index.js';
import type { AstroSettings, ComponentInstance, RoutesList } from '../types/astro.js';
import type { RewritePayload, RouteData, SSRLoadedRenderer, SSRManifest } from '../types/public/index.js';
/**
 * This Pipeline is used when the Vite SSR environment is runnable.
 */
export declare class RunnablePipeline extends Pipeline {
    readonly loader: ModuleLoader;
    readonly logger: Logger;
    readonly manifest: SSRManifest;
    readonly settings: AstroSettings;
    readonly getDebugInfo: () => Promise<string>;
    readonly defaultRoutes: {
        instance: ComponentInstance;
        matchesComponent(filePath: URL): boolean;
        route: string;
        component: string;
    }[];
    getName(): string;
    renderers: SSRLoadedRenderer[];
    routesList: RoutesList | undefined;
    private constructor();
    static create(manifestData: RoutesList, { loader, logger, manifest, settings, getDebugInfo, }: Pick<RunnablePipeline, 'loader' | 'logger' | 'manifest' | 'settings' | 'getDebugInfo'>): RunnablePipeline;
    headElements(routeData: RouteData): Promise<HeadElements>;
    componentMetadata(routeData: RouteData): Promise<Map<string, import("../types/public/internal.js").SSRComponentMetadata>>;
    preload(routeData: RouteData, filePath: URL): Promise<ComponentInstance>;
    clearRouteCache(): void;
    getComponentByRoute(routeData: RouteData): Promise<ComponentInstance>;
    tryRewrite(payload: RewritePayload, request: Request): Promise<TryRewriteResult>;
    setManifestData(manifestData: RoutesList): void;
}
