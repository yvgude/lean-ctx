import { ASTRO_VITE_ENVIRONMENT_NAMES } from "../core/constants.js";
import { VIRTUAL_PAGE_RESOLVED_MODULE_ID } from "../vite-plugin-pages/const.js";
import { getDevCssModuleNameFromPageVirtualModuleName } from "../vite-plugin-css/util.js";
function hmrReload() {
  return {
    name: "astro:hmr-reload",
    enforce: "post",
    hotUpdate: {
      order: "post",
      handler({ modules, server, timestamp }) {
        if (this.environment.name !== ASTRO_VITE_ENVIRONMENT_NAMES.ssr) return;
        let hasSsrOnlyModules = false;
        const invalidatedModules = /* @__PURE__ */ new Set();
        for (const mod of modules) {
          if (mod.id == null) continue;
          const clientModule = server.environments.client.moduleGraph.getModuleById(mod.id);
          if (clientModule != null) continue;
          this.environment.moduleGraph.invalidateModule(mod, invalidatedModules, timestamp, true);
          hasSsrOnlyModules = true;
        }
        for (const invalidatedModule of invalidatedModules) {
          if (invalidatedModule.id?.startsWith(VIRTUAL_PAGE_RESOLVED_MODULE_ID)) {
            const cssMod = this.environment.moduleGraph.getModuleById(
              getDevCssModuleNameFromPageVirtualModuleName(invalidatedModule.id)
            );
            if (!cssMod || cssMod.id == null) continue;
            this.environment.moduleGraph.invalidateModule(cssMod, void 0, timestamp, true);
          }
        }
        if (hasSsrOnlyModules) {
          server.ws.send({ type: "full-reload" });
          return [];
        }
      }
    }
  };
}
export {
  hmrReload as default
};
