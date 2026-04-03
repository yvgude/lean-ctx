import type { RouteData, SSRManifest } from '../types/public/internal.js';
import type { RunnablePipeline } from '../vite-plugin-app/pipeline.js';
type GetSortedPreloadedMatchesParams = {
    pipeline: RunnablePipeline;
    matches: RouteData[];
    manifest: SSRManifest;
};
export declare function getSortedPreloadedMatches({ pipeline, matches, manifest, }: GetSortedPreloadedMatchesParams): PreloadAndSetPrerenderStatusResult[];
type PreloadAndSetPrerenderStatusResult = {
    filePath: URL;
    route: RouteData;
};
export {};
