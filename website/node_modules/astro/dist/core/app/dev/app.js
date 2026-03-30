import { MiddlewareNoDataOrNextCalled, MiddlewareNotAResponse } from "../../errors/errors-data.js";
import { isAstroError } from "../../errors/index.js";
import {
  BaseApp
} from "../base.js";
import { NonRunnablePipeline } from "./pipeline.js";
import { getCustom404Route, getCustom500Route } from "../../routing/helpers.js";
import { ensure404Route } from "../../routing/astro-designed-error-pages.js";
import { matchRoute } from "../../routing/dev.js";
import { req } from "../../messages/runtime.js";
class DevApp extends BaseApp {
  logger;
  constructor(manifest, streaming = true, logger) {
    super(manifest, streaming, logger);
    this.logger = logger;
  }
  createPipeline(streaming, manifest, logger) {
    return NonRunnablePipeline.create({
      logger,
      manifest,
      streaming
    });
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
    ensure404Route(this.manifestData);
  }
  match(request) {
    return super.match(request, true);
  }
  async devMatch(pathname) {
    const matchedRoute = await matchRoute(
      pathname,
      this.manifestData,
      this.pipeline,
      this.manifest
    );
    if (!matchedRoute) return void 0;
    return {
      routeData: matchedRoute.route,
      resolvedPathname: matchedRoute.resolvedPathname
    };
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
          shouldInjectCspMetaTags: false
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
  DevApp
};
