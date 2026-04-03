//#region src/utils.d.ts
declare function defineFontProvider<Name extends string, Provider extends ProviderDefinition<never, never>>(name: Name, provider: Provider): Provider extends ProviderDefinition<infer Options, infer FamilyOptions> ? ProviderFactory<Name, Options, FamilyOptions> : never;
declare const formatMap: {
  woff2: string;
  woff: string;
  otf: string;
  ttf: string;
  eot: string;
};
//#endregion
//#region src/types.d.ts
type Awaitable$1<T> = T | Promise<T>;
interface ProviderContext {
  storage: {
    getItem: {
      <T = unknown>(key: string): Promise<T | null>;
      <T = unknown>(key: string, init: () => Awaitable$1<T>): Promise<T>;
    };
    setItem: (key: string, value: unknown) => Awaitable$1<void>;
  };
}
type FontStyles = 'normal' | 'italic' | 'oblique';
type FontFormat = keyof typeof formatMap;
interface ResolveFontOptions<FamilyOptions extends Record<string, any> | never = never> {
  weights: string[];
  styles: FontStyles[];
  subsets: string[];
  formats: FontFormat[];
  options?: [FamilyOptions] extends [never] ? undefined : FamilyOptions;
}
interface RemoteFontSource {
  url: string;
  originalURL?: string;
  format?: string;
  tech?: string;
}
interface LocalFontSource {
  name: string;
}
interface FontFaceMeta {
  /** The priority of the font face, usually used to indicate fallbacks. Smaller is more prioritized. */
  priority?: number;
  /**
   * The subset name of the font face. Many fonts provides font subsets such as latin, latin-ext, cyrillic, etc.
   */
  subset?: string;
  /**
   * A `RequestInit` object that should be used when fetching this font. This can be useful for
   * adding authorization headers and other metadata required for a font request.
   * @see https://developer.mozilla.org/en-US/docs/Web/API/RequestInit
   */
  init?: RequestInit;
}
interface FontFaceData {
  src: Array<LocalFontSource | RemoteFontSource>;
  /**
   * The font-display descriptor.
   * @default 'swap'
   */
  display?: 'auto' | 'block' | 'swap' | 'fallback' | 'optional';
  /** A font-weight value. */
  weight?: string | number | [number, number];
  /** A font-stretch value. */
  stretch?: string;
  /** A font-style value. */
  style?: string;
  /** The range of Unicode code points to be used from the font. */
  unicodeRange?: string[];
  /** Allows control over advanced typographic features in OpenType fonts. */
  featureSettings?: string;
  /** Allows low-level control over OpenType or TrueType font variations, by specifying the four letter axis names of the features to vary, along with their variation values. */
  variationSettings?: string;
  /** Metadata for the font face used by unifont */
  meta?: FontFaceMeta;
}
interface ResolveFontResult {
  /**
   * Return data used to generate @font-face declarations.
   * @see https://developer.mozilla.org/en-US/docs/Web/CSS/@font-face
   */
  fonts: FontFaceData[];
  fallbacks?: string[];
}
interface InitializedProvider<FamilyOptions extends Record<string, any> = never> {
  resolveFont: (family: string, options: ResolveFontOptions<FamilyOptions>) => Awaitable$1<ResolveFontResult | undefined>;
  listFonts?: (() => Awaitable$1<string[] | undefined>) | undefined;
}
interface ProviderDefinition<Options extends Record<string, any> = never, FamilyOptions extends Record<string, any> = never> {
  (options: Options, ctx: ProviderContext): Awaitable$1<InitializedProvider<FamilyOptions> | undefined>;
}
interface Provider<Name extends string = string, FamilyOptions extends Record<string, any> = never> {
  _name: Name;
  _options: unknown;
  (ctx: ProviderContext): Awaitable$1<InitializedProvider<FamilyOptions> | undefined>;
}
type ProviderFactory<Name extends string, Options extends Record<string, any> = never, FamilyOptions extends Record<string, any> = never> = [Options] extends [never] ? () => Provider<Name, FamilyOptions> : Partial<Options> extends Options ? (options?: Options) => Provider<Name, FamilyOptions> : (options: Options) => Provider<Name, FamilyOptions>;
//#endregion
//#region src/providers/google.d.ts
type VariableAxis = 'opsz' | 'slnt' | 'wdth' | (string & {});
interface GoogleProviderOptions {
  experimental?: {
    /**
     * Experimental: Setting variable axis configuration on a per-font basis.
     */
    variableAxis?: {
      [fontFamily: string]: Partial<Record<VariableAxis, ([string, string] | string)[]>>;
    };
    /**
     * Experimental: Specifying a list of glyphs to be included in the font for each font family.
     * This can reduce the size of the font file.
     */
    glyphs?: {
      [fontFamily: string]: string[];
    };
  };
}
interface GoogleFamilyOptions {
  experimental?: {
    /**
     * Experimental: Setting variable axis configuration on a per-font basis.
     */
    variableAxis?: Partial<Record<VariableAxis, ([string, string] | string)[]>>;
    /**
     * Experimental: Specifying a list of glyphs to be included in the font for each font family.
     * This can reduce the size of the font file.
     */
    glyphs?: string[];
  };
}
declare const _default$6: (options?: GoogleProviderOptions | undefined) => Provider<"google", GoogleFamilyOptions>;
//#endregion
//#region src/providers/googleicons.d.ts
interface GoogleiconsProviderOptions {
  experimental?: {
    /**
     * Experimental: Specifying a list of icons to be included in the font for each font family.
     * This can reduce the size of the font file.
     *
     * **Only available when resolving the new `Material Symbols` icons.**
     */
    glyphs?: {
      [fontFamily: string]: string[];
    };
  };
}
interface GoogleiconsFamilyOptions {
  experimental?: {
    /**
     * Experimental: Specifying a list of icons to be included in the font for each font family.
     * This can reduce the size of the font file.
     *
     * **Only available when resolving the new `Material Symbols` icons.**
     */
    glyphs?: string[];
  };
}
declare const _default$5: (options?: GoogleiconsProviderOptions | undefined) => Provider<"googleicons", GoogleiconsFamilyOptions>;
//#endregion
//#region src/providers/adobe.d.ts
interface AdobeProviderOptions {
  id: string[] | string;
}
declare const _default$4: (options: AdobeProviderOptions) => Provider<"adobe", never>;
//#endregion
//#region src/providers/bunny.d.ts
declare const _default$3: () => Provider<"bunny", never>;
//#endregion
//#region src/providers/fontshare.d.ts
declare const _default$2: () => Provider<"fontshare", never>;
//#endregion
//#region src/providers/fontsource.d.ts
declare const _default$1: () => Provider<"fontsource", never>;
//#endregion
//#region src/providers/npm.d.ts
interface NpmProviderOptions {
  /**
   * CDN to use for fetching npm packages remotely.
   * @default 'https://cdn.jsdelivr.net/npm'
   */
  cdn?: string;
  /**
   * Whether to fall back to fetching from the CDN when local resolution
   * fails or `readFile` is not provided.
   *
   * Set to `false` to only resolve from locally installed packages.
   * This is useful when another provider (e.g. `fontsource`) already
   * handles CDN resolution.
   *
   * @default true
   */
  remote?: boolean;
  /**
   * Optional function to read a file from the local filesystem.
   * When provided, the provider will try to resolve fonts from locally
   * installed packages in `node_modules` before falling back to the CDN
   * (unless `remote` is set to `false`).
   *
   * @example
   * ```ts
   * import { readFile } from 'node:fs/promises'
   * providers.npm({
   *   readFile: path => readFile(path, 'utf-8').catch(() => null),
   *   remote: false, // only resolve from local node_modules
   * })
   * ```
   */
  readFile?: (path: string) => Promise<string | null>;
  /**
   * Root directory of the project for resolving local packages.
   * Used to find `package.json` and `node_modules`.
   * @default '.' (current working directory)
   */
  root?: string;
}
interface NpmFamilyOptions {
  /**
   * The npm package name.
   * When not specified, the provider will try to find the font family
   * in known font package patterns (e.g. `@fontsource/${family}`).
   */
  package?: string;
  /**
   * The version of the package (used for CDN resolution only).
   * @default 'latest'
   */
  version?: string;
  /**
   * The entry CSS file to parse.
   * @default 'index.css'
   */
  file?: string;
}
declare const _default: (options?: NpmProviderOptions | undefined) => Provider<"npm", NpmFamilyOptions>;
declare namespace providers_d_exports {
  export { _default$4 as adobe, _default$3 as bunny, _default$2 as fontshare, _default$1 as fontsource, _default$6 as google, _default$5 as googleicons, _default as npm };
}
//#endregion
//#region src/cache.d.ts
type Awaitable<T> = T | Promise<T>;
type StorageValue = string | Record<string, unknown>;
interface Storage {
  getItem: (key: string) => Awaitable<any | null>;
  setItem: <T extends StorageValue = StorageValue>(key: string, value: T) => Awaitable<void>;
}
//#endregion
//#region src/unifont.d.ts
interface UnifontOptions {
  storage?: Storage;
  throwOnError?: boolean;
}
type ExtractFamilyOptions<T extends Provider> = Exclude<Parameters<NonNullable<Awaited<ReturnType<T>>>['resolveFont']>[1]['options'], undefined>;
interface Unifont<T extends Provider[]> {
  resolveFont: (fontFamily: string, options?: Partial<ResolveFontOptions<{ [K in T[number] as K['_name']]?: ExtractFamilyOptions<K> }>>, providers?: T[number]['_name'][]) => Promise<ResolveFontResult & {
    provider?: T[number]['_name'];
  }>;
  listFonts: (providers?: T[number]['_name'][]) => Promise<string[] | undefined>;
}
declare const defaultResolveOptions: ResolveFontOptions;
declare function createUnifont<T extends [Provider, ...Provider[]]>(providers: T, unifontOptions?: UnifontOptions): Promise<Unifont<T>>;
//#endregion
//#region src/index.d.ts
/** @deprecated */
type GoogleOptions = GoogleProviderOptions;
/** @deprecated */
type GoogleiconsOptions = GoogleiconsProviderOptions;
//#endregion
export { type AdobeProviderOptions, type FontFaceData, type FontFaceMeta, type FontStyles, type GoogleFamilyOptions, GoogleOptions, type GoogleProviderOptions, type GoogleiconsFamilyOptions, GoogleiconsOptions, type GoogleiconsProviderOptions, type InitializedProvider, type LocalFontSource, type NpmFamilyOptions, type NpmProviderOptions, type Provider, type ProviderContext, type ProviderDefinition, type ProviderFactory, type RemoteFontSource, type ResolveFontOptions, type ResolveFontResult, type Unifont, type UnifontOptions, createUnifont, defaultResolveOptions, defineFontProvider, providers_d_exports as providers };