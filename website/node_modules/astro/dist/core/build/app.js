import { BaseApp } from "../app/entrypoints/index.js";
import { BuildPipeline } from "./pipeline.js";
class BuildApp extends BaseApp {
  createPipeline(_streaming, manifest, ..._args) {
    return BuildPipeline.create({
      manifest
    });
  }
  async createRenderContext(payload) {
    return await super.createRenderContext({
      ...payload
    });
  }
  isDev() {
    return true;
  }
  setInternals(internals) {
    this.pipeline.setInternals(internals);
  }
  setOptions(options) {
    this.pipeline.setOptions(options);
    this.logger = options.logger;
  }
  getOptions() {
    return this.pipeline.getOptions();
  }
  getSettings() {
    return this.pipeline.getSettings();
  }
  async renderError(request, options) {
    if (options.status === 500) {
      if (options.response) {
        return options.response;
      }
      throw options.error;
    } else {
      return super.renderError(request, {
        ...options,
        prerenderedErrorPageFetch: void 0
      });
    }
  }
  getQueueStats() {
    if (this.pipeline.nodePool) {
      return this.pipeline.nodePool.getStats();
    }
  }
  logRequest(_options) {
  }
}
export {
  BuildApp
};
