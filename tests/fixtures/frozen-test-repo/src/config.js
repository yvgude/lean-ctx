const fs = require("fs");
const path = require("path");

const DEFAULTS = {
  port: 8080,
  host: "localhost",
  logLevel: "info",
  maxConnections: 100,
};

/**
 * Load configuration from a JSON file, merging with defaults.
 * @param {string} configPath
 * @returns {object}
 */
function loadConfig(configPath) {
  const resolved = path.resolve(configPath);
  if (!fs.existsSync(resolved)) {
    console.warn(`Config not found: ${resolved}, using defaults`);
    return { ...DEFAULTS };
  }
  const raw = fs.readFileSync(resolved, "utf-8");
  const user = JSON.parse(raw);
  return { ...DEFAULTS, ...user };
}

/**
 * Validate a configuration object.
 * @param {object} cfg
 * @returns {{ valid: boolean, errors: string[] }}
 */
function validateConfig(cfg) {
  const errors = [];
  if (typeof cfg.port !== "number" || cfg.port < 1 || cfg.port > 65535) {
    errors.push("port must be a number between 1 and 65535");
  }
  if (typeof cfg.host !== "string") {
    errors.push("host must be a string");
  }
  return { valid: errors.length === 0, errors };
}

module.exports = { loadConfig, validateConfig, DEFAULTS };
