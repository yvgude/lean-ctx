import { removeTrailingForwardSlash } from "@astrojs/internal-helpers/path";
import { BaseApp } from "../core/app/entrypoints/index.js";
import { shouldAppendForwardSlash } from "../core/build/util.js";
import { clientLocalsSymbol } from "../core/constants.js";
import {
  MiddlewareNoDataOrNextCalled,
  MiddlewareNotAResponse
} from "../core/errors/errors-data.js";
import { createSafeError, isAstroError } from "../core/errors/index.js";
import { createRequest } from "../core/request.js";
import { recordServerError } from "../vite-plugin-astro-server/error.js";
import { runWithErrorHandling } from "../vite-plugin-astro-server/index.js";
import { handle500Response, writeSSRResult } from "../vite-plugin-astro-server/response.js";
import { RunnablePipeline } from "./pipeline.js";
import { getCustom404Route, getCustom500Route } from "../core/routing/helpers.js";
import { ensure404Route } from "../core/routing/astro-designed-error-pages.js";
import { matchRoute } from "../core/routing/dev.js";
import { req } from "../core/messages/runtime.js";
class AstroServerApp extends BaseApp {
  settings;
  logger;
  loader;
  manifestData;
  currentRenderContext = void 0;
  constructor(manifest, streaming = true, logger, manifestData, loader, settings, getDebugInfo) {
    super(manifest, streaming, settings, logger, loader, manifestData, getDebugInfo);
    this.settings = settings;
    this.logger = logger;
    this.loader = loader;
    this.manifestData = manifestData;
  }
  isDev() {
    return true;
  }
  /**
   * Updates the routes list when files change during development.
   * Called via HMR when new pages are added/removed.
   */
  updateRoutes(newRoutesList) {
    this.manifestData = newRoutesList;
    this.pipeline.setManifestData(newRoutesList);
    ensure404Route(this.manifestData);
  }
  /**
   * Clears the route cache so that getStaticPaths() is re-evaluated.
   * Called via HMR when content collection data changes.
   */
  clearRouteCache() {
    this.pipeline.clearRouteCache();
  }
  async devMatch(pathname) {
    const matchedRoute = await matchRoute(
      pathname,
      this.manifestData,
      this.pipeline,
      this.manifest
    );
    if (!matchedRoute) {
      return void 0;
    }
    return {
      routeData: matchedRoute.route,
      resolvedPathname: matchedRoute.resolvedPathname
    };
  }
  static async create(manifest, routesList, logger, loader, settings, getDebugInfo) {
    return new AstroServerApp(manifest, true, logger, routesList, loader, settings, getDebugInfo);
  }
  createPipeline(_streaming, manifest, settings, logger, loader, manifestData, getDebugInfo) {
    const pipeline = RunnablePipeline.create(manifestData, {
      loader,
      logger,
      manifest,
      settings,
      getDebugInfo
    });
    return pipeline;
  }
  async createRenderContext(payload) {
    this.currentRenderContext = await super.createRenderContext(payload);
    return this.currentRenderContext;
  }
  async handleRequest({
    controller,
    incomingRequest,
    incomingResponse,
    isHttps
  }) {
    const origin = `${isHttps ? "https" : "http"}://${incomingRequest.headers[":authority"] ?? incomingRequest.headers.host}`;
    const url = new URL(origin + incomingRequest.url);
    let pathname;
    if (this.manifest.trailingSlash === "never" && !incomingRequest.url) {
      pathname = "";
    } else {
      pathname = decodeURI(url.pathname);
    }
    if (this.manifest.trailingSlash === "never" && pathname === "/" && this.manifest.base !== "/") {
      pathname = "";
    }
    url.pathname = removeTrailingForwardSlash(this.manifest.base) + url.pathname;
    if (url.pathname.endsWith("/") && !shouldAppendForwardSlash(this.manifest.trailingSlash, this.manifest.buildFormat)) {
      url.pathname = url.pathname.slice(0, -1);
    }
    let body = void 0;
    if (!(incomingRequest.method === "GET" || incomingRequest.method === "HEAD")) {
      let bytes = [];
      await new Promise((resolve) => {
        incomingRequest.on("data", (part) => {
          bytes.push(part);
        });
        incomingRequest.on("end", resolve);
      });
      body = Buffer.concat(bytes);
    }
    const self = this;
    await runWithErrorHandling({
      controller,
      pathname,
      async run() {
        const matchedRoute = await self.devMatch(pathname);
        if (!matchedRoute) {
          throw new Error("No route matched, and default 404 route was not found.");
        }
        const request = createRequest({
          url,
          headers: incomingRequest.headers,
          method: incomingRequest.method,
          body,
          logger: self.logger,
          isPrerendered: matchedRoute.routeData.prerender,
          routePattern: matchedRoute.routeData.component
        });
        const locals = Reflect.get(incomingRequest, clientLocalsSymbol);
        for (const [name, value] of Object.entries(self.settings.config.server.headers ?? {})) {
          if (value) incomingResponse.setHeader(name, value);
        }
        const clientAddress = incomingRequest.socket.remoteAddress;
        const response = await self.render(request, {
          locals,
          routeData: matchedRoute.routeData,
          clientAddress
        });
        await writeSSRResult(request, response, incomingResponse);
      },
      onError(_err) {
        const error = createSafeError(_err);
        if (self.loader) {
          const { errorWithMetadata } = recordServerError(
            self.loader,
            self.manifest,
            self.logger,
            error
          );
          handle500Response(self.loader, incomingResponse, errorWithMetadata);
        }
        return error;
      }
    });
  }
  match(request, _allowPrerenderedRoutes) {
    return super.match(request, true);
  }
  async renderError(request, {
    skipMiddleware = false,
    error,
    status,
    response: _response,
    ...resolvedRenderOptions
  }) {
    if (isAstroError(error) && [MiddlewareNoDataOrNextCalled.name, MiddlewareNotAResponse.name].includes(error.name)) {
      throw error;
    }
    const renderRoute = async (routeData) => {
      try {
        const preloadedComponent = await this.pipeline.getComponentByRoute(routeData);
        const renderContext = await this.createRenderContext({
          locals: resolvedRenderOptions.locals,
          pipeline: this.pipeline,
          pathname: this.getPathnameFromRequest(request),
          skipMiddleware,
          request,
          routeData,
          clientAddress: resolvedRenderOptions.clientAddress,
          status,
          shouldInjectCspMetaTags: !!this.manifest.csp
        });
        renderContext.props.error = error;
        const response = await renderContext.render(preloadedComponent);
        if (error) {
          this.logger.error("router", error.stack || error.message);
        }
        return response;
      } catch (_err) {
        if (skipMiddleware === false) {
          return this.renderError(request, {
            ...resolvedRenderOptions,
            status: 500,
            skipMiddleware: true,
            error: _err
          });
        }
        throw _err;
      }
    };
    if (status === 404) {
      const custom404 = getCustom404Route(this.manifestData);
      if (custom404) {
        return renderRoute(custom404);
      }
    }
    const custom500 = getCustom500Route(this.manifestData);
    if (!custom500) {
      throw error;
    } else {
      return renderRoute(custom500);
    }
  }
  logRequest({ pathname, method, statusCode, isRewrite, reqTime }) {
    if (pathname === "/favicon.ico") {
      return;
    }
    this.logger.info(
      null,
      req({
        url: pathname,
        method,
        statusCode,
        isRewrite,
        reqTime
      })
    );
  }
}
export {
  AstroServerApp
};
