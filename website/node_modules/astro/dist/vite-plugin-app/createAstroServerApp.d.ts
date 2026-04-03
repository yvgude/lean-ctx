import type http from 'node:http';
import { Logger } from '../core/logger/core.js';
import type { ModuleLoader } from '../core/module-loader/index.js';
import type { AstroSettings } from '../types/astro.js';
import type { DevServerController } from '../vite-plugin-astro-server/controller.js';
export default function createAstroServerApp(controller: DevServerController, settings: AstroSettings, loader: ModuleLoader, logger?: Logger): Promise<{
    handler(incomingRequest: http.IncomingMessage, incomingResponse: http.ServerResponse): void;
}>;
