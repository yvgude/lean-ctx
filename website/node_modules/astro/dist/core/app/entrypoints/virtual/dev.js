import { manifest } from "virtual:astro:manifest";
import { DevApp } from "../../dev/app.js";
import { createConsoleLogger } from "../../logging.js";
let currentDevApp = null;
const createApp = ({ streaming } = {}) => {
  const logger = createConsoleLogger(manifest.logLevel);
  currentDevApp = new DevApp(manifest, streaming, logger);
  if (import.meta.hot) {
    import.meta.hot.on("astro:routes-updated", async () => {
      if (!currentDevApp) return;
      try {
        const { routes: newRoutes } = await import("virtual:astro:routes");
        const newRoutesList = {
          routes: newRoutes.map((r) => r.routeData)
        };
        currentDevApp.updateRoutes(newRoutesList);
      } catch (e) {
        logger.error("router", `Failed to update routes via HMR:
 ${e}`);
      }
    });
    import.meta.hot.on("astro:content-changed", () => {
      if (!currentDevApp) return;
      currentDevApp.pipeline.routeCache.clearAll();
    });
  }
  return currentDevApp;
};
export {
  createApp
};
