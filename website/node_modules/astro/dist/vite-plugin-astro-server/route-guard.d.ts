import type * as vite from 'vite';
import type { AstroSettings } from '../types/astro.js';
/**
 * Middleware that prevents Vite from serving files that exist outside
 * of srcDir and publicDir when accessed via direct URL navigation.
 *
 * This fixes the issue where files like /README.md are served
 * when they exist at the project root but aren't part of Astro's routing.
 */
export declare function routeGuardMiddleware(settings: AstroSettings): vite.Connect.NextHandleFunction;
