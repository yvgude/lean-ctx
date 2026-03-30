# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

sitemap.js is a TypeScript library and CLI tool for generating sitemap XML files compliant with the sitemaps.org protocol. It supports streaming large datasets, handles sitemap indexes for >50k URLs, and includes parsers for reading existing sitemaps.

## Development Commands

### Building
```bash
npm run build                 # Compile TypeScript to dist/esm/ and dist/cjs/
npm run build:esm             # Build ESM only (dist/esm/)
npm run build:cjs             # Build CJS only (dist/cjs/)
```

### Testing
```bash
npm test                      # Run Jest tests with coverage
npm run test:full             # Run lint, build, Jest, and xmllint validation
npm run test:typecheck        # Type check only (tsc)
npm run test:perf             # Run performance tests (tests/perf.mjs)
npm run test:xmllint          # Validate XML schema (requires xmllint)
```

### Linting
```bash
npx eslint lib/* ./cli.ts     # Lint TypeScript files
npx eslint lib/* ./cli.ts --fix  # Auto-fix linting issues
```

### Running CLI Locally
```bash
node dist/esm/cli.js < urls.txt   # Run CLI from built dist
./dist/esm/cli.js --version       # Run directly (has shebang)
npm link && sitemap --version     # Link and test as global command
```

## Code Architecture

### Entry Points
- **[index.ts](index.ts)**: Main library entry point, exports all public APIs
- **[cli.ts](cli.ts)**: Command-line interface for generating/parsing sitemaps

### File Organization & Responsibilities

The library follows a strict separation of concerns. Each file has a specific purpose:

**Core Infrastructure:**
- **[lib/types.ts](lib/types.ts)**: ALL TypeScript type definitions, interfaces, and enums. NO implementation code.
- **[lib/constants.ts](lib/constants.ts)**: Single source of truth for all shared constants (limits, regexes, defaults).
- **[lib/validation.ts](lib/validation.ts)**: ALL validation logic, type guards, and validators centralized here.
- **[lib/utils.ts](lib/utils.ts)**: Stream utilities, URL normalization, and general helper functions.
- **[lib/errors.ts](lib/errors.ts)**: Custom error class definitions.
- **[lib/sitemap-xml.ts](lib/sitemap-xml.ts)**: Low-level XML generation utilities (text escaping, tag building).

**Stream Processing:**
- **[lib/sitemap-stream.ts](lib/sitemap-stream.ts)**: Main transform stream for URL → sitemap XML.
- **[lib/sitemap-item-stream.ts](lib/sitemap-item-stream.ts)**: Lower-level stream for sitemap item → XML elements.
- **[lib/sitemap-index-stream.ts](lib/sitemap-index-stream.ts)**: Streams for sitemap indexes and multi-file generation.

**Parsers:**
- **[lib/sitemap-parser.ts](lib/sitemap-parser.ts)**: Parses sitemap XML → SitemapItem objects.
- **[lib/sitemap-index-parser.ts](lib/sitemap-index-parser.ts)**: Parses sitemap index XML → IndexItem objects.

**High-Level API:**
- **[lib/sitemap-simple.ts](lib/sitemap-simple.ts)**: Simplified API for common use cases.

### Core Streaming Architecture

The library is built on Node.js Transform streams for memory-efficient processing of large URL lists:

**Stream Chain Flow:**
```
Input → Transform Stream → Output
```

**Key Stream Classes:**

1. **SitemapStream** ([lib/sitemap-stream.ts](lib/sitemap-stream.ts))
   - Core Transform stream that converts `SitemapItemLoose` objects to sitemap XML
   - Handles single sitemaps (up to ~50k URLs)
   - Automatically generates XML namespaces for images, videos, news, xhtml
   - Uses `SitemapItemStream` internally for XML element generation

2. **SitemapAndIndexStream** ([lib/sitemap-index-stream.ts](lib/sitemap-index-stream.ts))
   - Higher-level stream for handling >50k URLs
   - Automatically splits into multiple sitemap files when limit reached
   - Generates sitemap index XML pointing to individual sitemaps
   - Requires `getSitemapStream` callback to create output files

3. **SitemapItemStream** ([lib/sitemap-item-stream.ts](lib/sitemap-item-stream.ts))
   - Low-level Transform stream that converts sitemap items to XML elements
   - Validates and normalizes URLs
   - Handles image, video, news, and link extensions

4. **XMLToSitemapItemStream** ([lib/sitemap-parser.ts](lib/sitemap-parser.ts))
   - Parser that converts sitemap XML back to `SitemapItem` objects
   - Built on SAX parser for streaming large XML files

5. **SitemapIndexStream** ([lib/sitemap-index-stream.ts](lib/sitemap-index-stream.ts))
   - Generates sitemap index XML from a list of sitemap URLs
   - Used for organizing multiple sitemaps

### Type System

**[lib/types.ts](lib/types.ts)** defines the core data structures:

- **SitemapItemLoose**: Flexible input type (accepts strings, objects, arrays for images/videos)
- **SitemapItem**: Strict normalized type (arrays only)
- **ErrorLevel**: Enum controlling validation behavior (SILENT, WARN, THROW)
- **NewsItem**, **Img**, **VideoItem**, **LinkItem**: Extension types for rich sitemap entries
- **IndexItem**: Structure for sitemap index entries
- **StringObj**: Generic object with string keys (used for XML attributes)

### Constants & Limits

**[lib/constants.ts](lib/constants.ts)** is the single source of truth for:
- `LIMITS`: Security limits (max URL length, max items per sitemap, max video tags, etc.)
- `DEFAULT_SITEMAP_ITEM_LIMIT`: Default items per sitemap file (45,000)

All limits are documented with references to sitemaps.org and Google specifications.

### Validation & Normalization

**[lib/validation.ts](lib/validation.ts)** centralizes ALL validation logic:
- `validateSMIOptions()`: Validates complete sitemap item fields
- `validateURL()`, `validatePath()`, `validateLimit()`: Input validation
- `validators`: Regex patterns for field validation (price, language, genres, etc.)
- Type guards: `isPriceType()`, `isResolution()`, `isValidChangeFreq()`, `isValidYesNo()`, `isAllowDeny()`

**[lib/utils.ts](lib/utils.ts)** contains utility functions:
- `normalizeURL()`: Converts `SitemapItemLoose` to `SitemapItem` with validation
- `lineSeparatedURLsToSitemapOptions()`: Stream transform for parsing line-delimited URLs
- `ReadlineStream`: Helper for reading line-by-line input
- `mergeStreams()`: Combines multiple streams into one

### XML Generation

**[lib/sitemap-xml.ts](lib/sitemap-xml.ts)** provides low-level XML building functions:
- Tag generation helpers (`otag`, `ctag`, `element`)
- Sitemap-specific element builders (images, videos, news, links)

### Error Handling

**[lib/errors.ts](lib/errors.ts)** defines custom error classes:
- `EmptyStream`, `EmptySitemap`: Stream validation errors
- `InvalidAttr`, `InvalidVideoFormat`, `InvalidNewsFormat`: Validation errors
- `XMLLintUnavailable`: External tool errors

## When Making Changes

### Where to Add New Code

- **New type or interface?** → Add to [lib/types.ts](lib/types.ts)
- **New constant or limit?** → Add to [lib/constants.ts](lib/constants.ts) (import from here everywhere)
- **New validation function or type guard?** → Add to [lib/validation.ts](lib/validation.ts)
- **New utility function?** → Add to [lib/utils.ts](lib/utils.ts)
- **New error class?** → Add to [lib/errors.ts](lib/errors.ts)
- **New public API?** → Export from [index.ts](index.ts)

### Common Pitfalls to Avoid

1. **DON'T duplicate constants** - Always import from [lib/constants.ts](lib/constants.ts)
2. **DON'T define types in implementation files** - Put them in [lib/types.ts](lib/types.ts)
3. **DON'T scatter validation logic** - Keep it all in [lib/validation.ts](lib/validation.ts)
4. **DON'T break backward compatibility** - Use re-exports if moving code between files
5. **DO update [index.ts](index.ts)** if adding new public API functions

### Adding a New Field to Sitemap Items

1. Add type to [lib/types.ts](lib/types.ts) in both `SitemapItem` and `SitemapItemLoose` interfaces
2. Add XML generation logic in [lib/sitemap-item-stream.ts](lib/sitemap-item-stream.ts) `_transform` method
3. Add parsing logic in [lib/sitemap-parser.ts](lib/sitemap-parser.ts) SAX event handlers
4. Add validation in [lib/validation.ts](lib/validation.ts) `validateSMIOptions` if needed
5. Add constants to [lib/constants.ts](lib/constants.ts) if limits are needed
6. Write tests covering the new field

### Before Submitting Changes

```bash
npm run test:full    # Run all tests, linting, and validation
npm run build        # Ensure both ESM and CJS builds work
npm test             # Verify 90%+ code coverage maintained
```

## Finding Code in the Codebase

### "Where is...?"

- **Validation for sitemap items?** → [lib/validation.ts](lib/validation.ts) (`validateSMIOptions`)
- **URL validation?** → [lib/validation.ts](lib/validation.ts) (`validateURL`)
- **Constants like max URL length?** → [lib/constants.ts](lib/constants.ts) (`LIMITS`)
- **Type guards (isPriceType, isValidYesNo)?** → [lib/validation.ts](lib/validation.ts)
- **Type definitions (SitemapItem, etc)?** → [lib/types.ts](lib/types.ts)
- **XML escaping/generation?** → [lib/sitemap-xml.ts](lib/sitemap-xml.ts)
- **URL normalization?** → [lib/utils.ts](lib/utils.ts) (`normalizeURL`)
- **Stream utilities?** → [lib/utils.ts](lib/utils.ts) (`mergeStreams`, `lineSeparatedURLsToSitemapOptions`)

### "How do I...?"

- **Check if a value is valid?** → Import type guard from [lib/validation.ts](lib/validation.ts)
- **Get a constant limit?** → Import `LIMITS` from [lib/constants.ts](lib/constants.ts)
- **Validate user input?** → Use validation functions from [lib/validation.ts](lib/validation.ts)
- **Generate XML safely?** → Use functions from [lib/sitemap-xml.ts](lib/sitemap-xml.ts) (auto-escapes)

## Testing Strategy

Tests are in [tests/](tests/) directory with Jest:
- **[tests/sitemap-stream.test.ts](tests/sitemap-stream.test.ts)**: Core streaming functionality
- **[tests/sitemap-parser.test.ts](tests/sitemap-parser.test.ts)**: XML parsing
- **[tests/sitemap-index.test.ts](tests/sitemap-index.test.ts)**: Index generation
- **[tests/sitemap-simple.test.ts](tests/sitemap-simple.test.ts)**: High-level API
- **[tests/cli.test.ts](tests/cli.test.ts)**: CLI argument parsing
- **[tests/*-security.test.ts](tests/)**: Security-focused validation and injection tests
- **[tests/sitemap-utils.test.ts](tests/sitemap-utils.test.ts)**: Utility function tests

### Coverage Requirements (enforced by jest.config.cjs)
- Branches: 80%
- Functions: 90%
- Lines: 90%
- Statements: 90%

### When to Write Tests
- **Always** write tests for new validation functions
- **Always** write tests for new security features
- **Always** add security tests for user-facing inputs (URL validation, path traversal, etc.)
- Write tests for bug fixes to prevent regression
- Add edge case tests for data transformations

## TypeScript Configuration

The project uses a dual-build setup for ESM and CommonJS:

- **[tsconfig.json](tsconfig.json)**: ESM build (`module: "NodeNext"`, `moduleResolution: "NodeNext"`)
  - Outputs to `dist/esm/`
  - Includes both [index.ts](index.ts) and [cli.ts](cli.ts)
  - ES2023 target with strict null checks enabled

- **[tsconfig.cjs.json](tsconfig.cjs.json)**: CommonJS build (`module: "CommonJS"`)
  - Outputs to `dist/cjs/`
  - Excludes [cli.ts](cli.ts) (CLI is ESM-only)
  - Only includes [index.ts](index.ts) for library exports

**Important**: All relative imports must include `.js` extensions for ESM compatibility (e.g., `import { foo } from './types.js'`)

## Key Patterns

### Stream Creation
Always create a new stream instance per operation. Streams cannot be reused.

```typescript
const stream = new SitemapStream({ hostname: 'https://example.com' });
stream.write({ url: '/page' });
stream.end();
```

### Memory Management
For large datasets, use streaming patterns with `pipe()` rather than collecting all data in memory:

```typescript
// Good - streams through
lineSeparatedURLsToSitemapOptions(readStream).pipe(sitemapStream).pipe(outputStream);

// Bad - loads everything into memory
const allUrls = await readAllUrls();
allUrls.forEach(url => stream.write(url));
```

### Error Levels
Control validation strictness with `ErrorLevel`:
- `SILENT`: Skip validation (fastest, use in production if data is pre-validated)
- `WARN`: Log warnings (default, good for development)
- `THROW`: Throw on invalid data (strict mode, good for testing)

## Package Distribution

The package is distributed as a dual ESM/CommonJS package with `"type": "module"` in package.json:

- **ESM**: `dist/esm/index.js` (ES modules)
- **CJS**: `dist/cjs/index.js` (CommonJS, via conditional exports)
- **Types**: `dist/esm/index.d.ts` (TypeScript definitions)
- **Binary**: `dist/esm/cli.js` (ESM-only CLI, executable via `npx sitemap`)
- **Engines**: Node.js >=20.19.5, npm >=10.8.2

### Dual Package Exports

The `exports` field in package.json provides conditional exports:

```json
{
  "exports": {
    ".": {
      "import": "./dist/esm/index.js",
      "require": "./dist/cjs/index.js"
    }
  }
}
```

This allows both:
```javascript
// ESM
import { SitemapStream } from 'sitemap'

// CommonJS
const { SitemapStream } = require('sitemap')
```

## Git Hooks

Husky pre-commit hooks run lint-staged which:
- Sorts package.json
- Runs eslint --fix on TypeScript files
- Runs prettier on TypeScript files

## Architecture Decisions

### Why This File Structure?

The codebase is organized around **separation of concerns** and **single source of truth** principles:

1. **Types in [lib/types.ts](lib/types.ts)**: All interfaces and enums live here, with NO implementation code. This makes types easy to find and prevents circular dependencies.

2. **Constants in [lib/constants.ts](lib/constants.ts)**: All shared constants (limits, regexes) defined once. This prevents inconsistencies where different files use different values.

3. **Validation in [lib/validation.ts](lib/validation.ts)**: All validation logic centralized. Easy to find, test, and maintain security rules.

4. **Clear file boundaries**: Each file has ONE responsibility. You know exactly where to look for specific functionality.

### Key Principles

- **Single Source of Truth**: Constants and validation logic exist in exactly one place
- **No Duplication**: Import shared code rather than copying it
- **Backward Compatibility**: Use re-exports when moving code between files to avoid breaking changes
- **Types Separate from Implementation**: [lib/types.ts](lib/types.ts) contains only type definitions
- **Security First**: All validation and limits are centralized for consistent security enforcement

### Benefits of This Organization

- **Discoverability**: Developers know exactly where to look for types, constants, or validation
- **Maintainability**: Changes to limits or validation only require editing one file
- **Consistency**: Importing from a single source prevents different parts of the code using different limits
- **Testing**: Centralized validation makes it easy to write comprehensive security tests
- **Refactoring**: Clear boundaries make it safe to refactor without affecting other modules
