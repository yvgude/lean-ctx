import type { RoutesList } from '../../types/astro.js';
import type { RemotePattern, RouteData } from '../../types/public/index.js';
import type { Pipeline } from '../base-pipeline.js';
import { getSetCookiesFromResponse } from '../cookies/index.js';
import { AstroIntegrationLogger, Logger } from '../logger/core.js';
import { type CreateRenderContext, RenderContext } from '../render-context.js';
import type { AppPipeline } from './pipeline.js';
import type { SSRManifest } from './types.js';
export interface DevMatch {
    routeData: RouteData;
    resolvedPathname: string;
}
export interface RenderOptions {
    /**
     * Whether to automatically add all cookies written by `Astro.cookie.set()` to the response headers.
     *
     * When set to `true`, they will be added to the `Set-Cookie` header as comma-separated key=value pairs. You can use the standard `response.headers.getSetCookie()` API to read them individually.
     *
     * When set to `false`, the cookies will only be available from `App.getSetCookieFromResponse(response)`.
     *
     * @default {false}
     */
    addCookieHeader?: boolean;
    /**
     * The client IP address that will be made available as `Astro.clientAddress` in pages, and as `ctx.clientAddress` in API routes and middleware.
     *
     * Default: `request[Symbol.for("astro.clientAddress")]`
     */
    clientAddress?: string;
    /**
     * The mutable object that will be made available as `Astro.locals` in pages, and as `ctx.locals` in API routes and middleware.
     */
    locals?: object;
    /**
     * A custom fetch function for retrieving prerendered pages - 404 or 500.
     *
     * If not provided, Astro will fall back to its default behavior for fetching error pages.
     *
     * When a dynamic route is matched but ultimately results in a 404, this function will be used
     * to fetch the prerendered 404 page if available. Similarly, it may be used to fetch a
     * prerendered 500 error page when necessary.
     *
     * @param {ErrorPagePath} url - The URL of the prerendered 404 or 500 error page to fetch.
     * @returns {Promise<Response>} A promise resolving to the prerendered response.
     */
    prerenderedErrorPageFetch?: (url: ErrorPagePath) => Promise<Response>;
    /**
     * **Advanced API**: you probably do not need to use this.
     *
     * Default: `app.match(request)`
     */
    routeData?: RouteData;
}
type RequiredRenderOptions = Required<RenderOptions>;
interface ResolvedRenderOptions {
    addCookieHeader: RequiredRenderOptions['addCookieHeader'];
    clientAddress: RequiredRenderOptions['clientAddress'] | undefined;
    prerenderedErrorPageFetch: RequiredRenderOptions['prerenderedErrorPageFetch'] | undefined;
    locals: RequiredRenderOptions['locals'] | undefined;
    routeData: RequiredRenderOptions['routeData'] | undefined;
}
export interface RenderErrorOptions extends ResolvedRenderOptions {
    response?: Response;
    status: 404 | 500;
    /**
     * Whether to skip middleware while rendering the error page. Defaults to false.
     */
    skipMiddleware?: boolean;
    /**
     * Allows passing an error to 500.astro. It will be available through `Astro.props.error`.
     */
    error?: unknown;
}
type ErrorPagePath = `${string}/404` | `${string}/500` | `${string}/404/` | `${string}/500/` | `${string}404.html` | `${string}500.html`;
export declare abstract class BaseApp<P extends Pipeline = AppPipeline> {
    #private;
    manifest: SSRManifest;
    manifestData: RoutesList;
    pipeline: P;
    adapterLogger: AstroIntegrationLogger;
    baseWithoutTrailingSlash: string;
    logger: Logger;
    constructor(manifest: SSRManifest, streaming?: boolean, ...args: any[]);
    abstract isDev(): boolean;
    createRenderContext(payload: CreateRenderContext): Promise<RenderContext>;
    getAdapterLogger(): AstroIntegrationLogger;
    getAllowedDomains(): Partial<RemotePattern>[] | undefined;
    protected matchesAllowedDomains(forwardedHost: string, protocol?: string): boolean;
    static validateForwardedHost(forwardedHost: string, allowedDomains?: Partial<RemotePattern>[], protocol?: string): boolean;
    /**
     * Creates a pipeline by reading the stored manifest
     *
     * @param streaming
     * @param manifest
     * @param args
     * @private
     */
    abstract createPipeline(streaming: boolean, manifest: SSRManifest, ...args: any[]): P;
    set setManifestData(newManifestData: RoutesList);
    removeBase(pathname: string): string;
    /**
     * It removes the base from the request URL, prepends it with a forward slash and attempts to decoded it.
     *
     * If the decoding fails, it logs the error and return the pathname as is.
     * @param request
     */
    getPathnameFromRequest(request: Request): string;
    /**
     * Given a `Request`, it returns the `RouteData` that matches its `pathname`. By default, prerendered
     * routes aren't returned, even if they are matched.
     *
     * When `allowPrerenderedRoutes` is `true`, the function returns matched prerendered routes too.
     * @param request
     * @param allowPrerenderedRoutes
     */
    match(request: Request, allowPrerenderedRoutes?: boolean): RouteData | undefined;
    private createRouter;
    /**
     * A matching route function to use in the development server.
     * Contrary to the `.match` function, this function resolves props and params, returning the correct
     * route based on the priority, segments. It also returns the correct, resolved pathname.
     * @param pathname
     */
    devMatch(pathname?: string): Promise<DevMatch | undefined> | undefined;
    private computePathnameFromDomain;
    private redirectTrailingSlash;
    render(request: Request, { addCookieHeader, clientAddress, locals, prerenderedErrorPageFetch, routeData, }?: RenderOptions): Promise<Response>;
    setCookieHeaders(response: Response): Generator<string, string[], any>;
    /**
     * Reads all the cookies written by `Astro.cookie.set()` onto the passed response.
     * For example,
     * ```ts
     * for (const cookie_ of App.getSetCookieFromResponse(response)) {
     *     const cookie: string = cookie_
     * }
     * ```
     * @param response The response to read cookies from.
     * @returns An iterator that yields key-value pairs as equal-sign-separated strings.
     */
    static getSetCookieFromResponse: typeof getSetCookiesFromResponse;
    /**
     * If it is a known error code, try sending the according page (e.g. 404.astro / 500.astro).
     * This also handles pre-rendered /404 or /500 routes
     */
    renderError(request: Request, { status, response: originalResponse, skipMiddleware, error, ...resolvedRenderOptions }: RenderErrorOptions): Promise<Response>;
    private mergeResponses;
    getDefaultStatusCode(routeData: RouteData, pathname: string): number;
    getManifest(): SSRManifest;
    logThisRequest({ pathname, method, statusCode, isRewrite, timeStart, }: {
        pathname: string;
        method: string;
        statusCode: number;
        isRewrite: boolean;
        timeStart: number;
    }): void;
    abstract logRequest(_options: LogRequestPayload): void;
}
export type LogRequestPayload = {
    /**
     * The current path being rendered
     */
    pathname: string;
    /**
     * The method of the request
     */
    method: string;
    /**
     * The status code of the request
     */
    statusCode: number;
    /**
     * If the current request is a rewrite
     */
    isRewrite: boolean;
    /**
     * How long it took to render the request
     */
    reqTime: number;
};
export {};
