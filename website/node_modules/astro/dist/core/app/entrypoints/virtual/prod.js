import { manifest } from "virtual:astro:manifest";
import { App } from "../../app.js";
const createApp = ({ streaming } = {}) => {
  return new App(manifest, streaming);
};
export {
  createApp
};
