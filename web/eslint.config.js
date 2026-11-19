import i18next from "eslint-plugin-i18next";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "node_modules"] },
  {
    files: ["src/**/*.{ts,tsx}"],
    extends: [...tseslint.configs.recommended],
    languageOptions: { ecmaVersion: 2022, globals: globals.browser },
    plugins: { "react-hooks": reactHooks, i18next },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // docs/PLAN.md, section 6: no user-facing string is hard-coded in a component.
      // Flags any text between JSX tags and any literal in an attribute a user reads.
      "i18next/no-literal-string": [
        "error",
        {
          mode: "jsx-only",
          "jsx-attributes": { include: ["title", "alt", "placeholder", "aria-label", "aria-description", "label"] },
          // Separators and symbols that are the same in every language.
          words: { exclude: ["[0-9!-/:-@[-`{-~\\s·•—–]+"] },
        },
      ],
    },
  },
);
