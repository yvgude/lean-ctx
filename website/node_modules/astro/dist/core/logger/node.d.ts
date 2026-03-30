import type { AstroInlineConfig } from '../../types/public/config.js';
import { Logger } from './core.js';
import { type LogMessage, type LogWritable } from './core.js';
export declare const nodeLogDestination: LogWritable<LogMessage>;
export declare function enableVerboseLogging(): void;
export declare function createNodeLogger(inlineConfig: AstroInlineConfig): Logger;
