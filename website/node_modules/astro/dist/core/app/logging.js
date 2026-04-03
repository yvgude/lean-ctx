import { consoleLogDestination } from "../logger/console.js";
import { Logger } from "../logger/core.js";
function createConsoleLogger(level) {
  return new Logger({
    dest: consoleLogDestination,
    level: level ?? "info"
  });
}
export {
  createConsoleLogger
};
