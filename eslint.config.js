// @ts-check
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  {
    // src/bindings is ts-rs-generated; if lint flags it, ignore the directory rather than
    // editing generated output.
    ignores: ["dist", "src-tauri/target", "src-tauri/gen", "src/bindings"],
  },
  tseslint.configs.recommended,
  reactHooks.configs.flat.recommended,
);
