import { createDebug, enable as obugEnable } from "obug";
import { Logger } from "./core.js";
import { getEventPrefix, levels } from "./core.js";
const nodeLogDestination = {
  write(event) {
    let dest = process.stderr;
    if (levels[event.level] < levels["error"]) {
      dest = process.stdout;
    }
    let trailingLine = event.newLine ? "\n" : "";
    if (event.label === "SKIP_FORMAT") {
      dest.write(event.message + trailingLine);
    } else {
      dest.write(getEventPrefix(event) + " " + event.message + trailingLine);
    }
    return true;
  }
};
const debuggers = {};
function debug(type, ...messages) {
  const namespace = `astro:${type}`;
  debuggers[namespace] = debuggers[namespace] || createDebug(namespace);
  return debuggers[namespace](...messages);
}
globalThis._astroGlobalDebug = debug;
function enableVerboseLogging() {
  obugEnable("astro:*,vite:*");
  debug("cli", '--verbose flag enabled! Enabling: DEBUG="astro:*,vite:*"');
  debug(
    "cli",
    'Tip: Set the DEBUG env variable directly for more control. Example: "DEBUG=astro:*,vite:* astro build".'
  );
}
function createNodeLogger(inlineConfig) {
  if (inlineConfig.logger) return inlineConfig.logger;
  return new Logger({
    dest: nodeLogDestination,
    level: inlineConfig.logLevel ?? "info"
  });
}
export {
  createNodeLogger,
  enableVerboseLogging,
  nodeLogDestination
};
