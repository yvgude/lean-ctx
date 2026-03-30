import type { Params } from '../../types/public/common.js';
import type { APIContext } from '../../types/public/context.js';
import { sequence } from './sequence.js';
/**
 * Payload for creating a context to be passed to Astro middleware
 */
export type CreateContext = {
    /**
     * The incoming request
     */
    request: Request;
    /**
     * Optional parameters
     */
    params?: Params;
    /**
     * A list of locales that are supported by the user
     */
    userDefinedLocales?: string[];
    /**
     * User defined default locale
     */
    defaultLocale: string;
    /**
     * Initial value of the locals
     */
    locals?: App.Locals;
    /**
     * The client IP address. Must be provided by the adapter or platform from a
     * trusted source (e.g. socket address, platform-provided header).
     *
     * If not provided, accessing `context.clientAddress` will throw an error.
     */
    clientAddress?: string;
};
/**
 * Creates a context to be passed to Astro middleware `onRequest` function.
 */
declare function createContext({ request, params, userDefinedLocales, defaultLocale, locals, clientAddress, }: CreateContext): APIContext;
/**
 * It attempts to serialize `value` and return it as a string.
 *
 * ## Errors
 *  If the `value` is not serializable if the function will throw a runtime error.
 *
 * Something is **not serializable** when it contains properties/values like functions, `Map`, `Set`, `Date`,
 * and other types that can't be made a string.
 *
 * @param value
 */
declare function trySerializeLocals(value: unknown): string;
export { createContext, sequence, trySerializeLocals };
export { defineMiddleware } from './defineMiddleware.js';
