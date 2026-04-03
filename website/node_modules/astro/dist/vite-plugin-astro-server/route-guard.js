import * as fs from "node:fs";
import { notFoundTemplate } from "../template/4xx.js";
import { writeHtmlResponse } from "./response.js";
const VITE_INTERNAL_PREFIXES = [
  "/@vite/",
  "/@fs/",
  "/@id/",
  "/__vite",
  "/@react-refresh",
  "/node_modules/",
  "/.astro/"
];
function routeGuardMiddleware(settings) {
  const { config } = settings;
  return function devRouteGuard(req, res, next) {
    const url = req.url;
    if (!url) {
      return next();
    }
    const accept = req.headers.accept || "";
    if (!accept.includes("text/html")) {
      return next();
    }
    let pathname;
    try {
      pathname = decodeURI(new URL(url, "http://localhost").pathname);
    } catch {
      return next();
    }
    if (VITE_INTERNAL_PREFIXES.some((prefix) => pathname.startsWith(prefix))) {
      return next();
    }
    if (url.includes("?")) {
      return next();
    }
    const publicFilePath = new URL("." + pathname, config.publicDir);
    if (fs.existsSync(publicFilePath)) {
      return next();
    }
    const srcFilePath = new URL("." + pathname, config.srcDir);
    if (fs.existsSync(srcFilePath)) {
      return next();
    }
    const rootFilePath = new URL("." + pathname, config.root);
    try {
      const stat = fs.statSync(rootFilePath);
      if (stat.isFile()) {
        const html = notFoundTemplate(pathname);
        return writeHtmlResponse(res, 404, html);
      }
    } catch {
    }
    return next();
  };
}
export {
  routeGuardMiddleware
};
