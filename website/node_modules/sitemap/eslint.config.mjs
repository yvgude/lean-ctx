import { defineConfig, globalIgnores } from "eslint/config";
import jest from "eslint-plugin-jest";
import typescriptEslint from "@typescript-eslint/eslint-plugin";
import globals from "globals";
import tsParser from "@typescript-eslint/parser";
import path from "node:path";
import { fileURLToPath } from "node:url";
import js from "@eslint/js";
import { FlatCompat } from "@eslint/eslintrc";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const compat = new FlatCompat({
    baseDirectory: __dirname,
    recommendedConfig: js.configs.recommended,
    allConfig: js.configs.all
});

export default defineConfig([globalIgnores([
    "test/",
    "**/__test__",
    "**/__tests__",
    "**/node_modules",
    "node_modules/",
    "**/node_modules/",
    "**/.idea",
    "**/.nyc_output",
    "**/coverage",
    "**/*.d.ts",
    "bin/**/*",
]), {
    extends: compat.extends(
        "eslint:recommended",
        "plugin:@typescript-eslint/eslint-recommended",
        "plugin:@typescript-eslint/recommended",
        "prettier",
        "plugin:prettier/recommended",
    ),

    plugins: {
        jest,
        "@typescript-eslint": typescriptEslint,
    },

    languageOptions: {
        globals: {
            ...globals.jest,
            ...globals.node,
        },

        parser: tsParser,
        ecmaVersion: 2023,
        sourceType: "module",
    },

    rules: {
        indent: "off",

        "lines-between-class-members": ["error", "always", {
            exceptAfterSingleLine: true,
        }],

        "no-case-declarations": 0,
        "no-console": 0,
        "no-dupe-class-members": "off",
        "no-unused-vars": 0,

        "padding-line-between-statements": ["error", {
            blankLine: "always",
            prev: "multiline-expression",
            next: "multiline-expression",
        }],

        "@typescript-eslint/ban-ts-comment": ["error", {
            "ts-expect-error": "allow-with-description",
        }],

        "@typescript-eslint/explicit-member-accessibility": "off",

        "@typescript-eslint/naming-convention": ["error", {
            selector: "default",
            format: null,
        }, {
            selector: "interface",
            prefix: [],
            format: null,
        }],

        "@typescript-eslint/no-parameter-properties": "off",

        "@typescript-eslint/no-unused-vars": ["error", {
            args: "none",
        }],
    },
}, {
    files: ["**/*.js"],

    rules: {
        "@typescript-eslint/explicit-function-return-type": "off",
        "@typescript-eslint/no-var-requires": "off",
    },
}]);
