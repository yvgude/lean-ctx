import type { ActionAPIContext } from '../actions/runtime/types.js';
import type { ComponentInstance } from '../types/astro.js';
import type { MiddlewareHandler, Props } from '../types/public/common.js';
import type { APIContext, AstroGlobal } from '../types/public/context.js';
import type { RouteData, SSRResult } from '../types/public/internal.js';
import type { ServerIslandMappings, SSRActions } from './app/types.js';
import { AstroCookies } from './cookies/index.js';
import { type Pipeline } from './render/index.js';
import { type CacheLike } from './cache/runtime/cache.js';
import { AstroSession } from './session/runtime.js';
/**
 * Each request is rendered using a `RenderContext`.
 * It contains data unique to each request. It is responsible for executing middleware, calling endpoints, and rendering the page by gathering necessary data from a `Pipeline`.
 */
export type CreateRenderContext = Pick<RenderContext, 'pathname' | 'pipeline' | 'request' | 'routeData' | 'clientAddress'> & Partial<Pick<RenderContext, 'locals' | 'status' | 'props' | 'partial' | 'actions' | 'shouldInjectCspMetaTags' | 'skipMiddleware'>>;
export declare class RenderContext {
    #private;
    readonly pipeline: Pipeline;
    locals: App.Locals;
    readonly middleware: MiddlewareHandler;
    readonly actions: SSRActions;
    readonly serverIslands: ServerIslandMappings;
    pathname: string;
    request: Request;
    routeData: RouteData;
    status: number;
    clientAddress: string | undefined;
    protected cookies: AstroCookies;
    params: import("../types/public/common.js").Params;
    protected url: URL;
    props: Props;
    partial: undefined | boolean;
    shouldInjectCspMetaTags: boolean;
    session: AstroSession | undefined;
    cache: CacheLike;
    skipMiddleware: boolean;
    private constructor();
    /**
     * A flag that tells the render content if the rewriting was triggered
     */
    isRewriting: boolean;
    /**
     * A safety net in case of loops
     */
    counter: number;
    result: SSRResult | undefined;
    static create({ locals, pathname, pipeline, request, routeData, clientAddress, status, props, partial, shouldInjectCspMetaTags, skipMiddleware, }: CreateRenderContext): Promise<RenderContext>;
    /**
     * The main function of the RenderContext.
     *
     * Use this function to render any route known to Astro.
     * It attempts to render a route. A route can be a:
     *
     * - page
     * - redirect
     * - endpoint
     * - fallback
     */
    render(componentInstance: ComponentInstance | undefined, slots?: Record<string, any>): Promise<Response>;
    createAPIContext(props: APIContext['props'], context: ActionAPIContext): APIContext;
    createActionAPIContext(): ActionAPIContext;
    createResult(mod: ComponentInstance, ctx: ActionAPIContext): Promise<SSRResult>;
    /**
     * The Astro global is sourced in 3 different phases:
     * - **Static**: `.generator` and `.glob` is printed by the compiler, instantiated once per process per astro file
     * - **Page-level**: `.request`, `.cookies`, `.locals` etc. These remain the same for the duration of the request.
     * - **Component-level**: `.props`, `.slots`, and `.self` are unique to each _use_ of each component.
     *
     * The page level partial is used as the prototype of the user-visible `Astro` global object, which is instantiated once per use of a component.
     */
    createAstro(result: SSRResult, props: Record<string, any>, slotValues: Record<string, any> | null, apiContext: ActionAPIContext): AstroGlobal;
    createAstroPagePartial(result: SSRResult, apiContext: ActionAPIContext): Omit<AstroGlobal, 'props' | 'self' | 'slots'>;
    getClientAddress(): string;
    computeCurrentLocale(): string | undefined;
    computePreferredLocale(): string | undefined;
    computePreferredLocaleList(): string[] | undefined;
}
