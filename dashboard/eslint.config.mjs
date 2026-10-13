// ESLint's size rules (ADR-0172), shared with the other front end from packages/shared.
import { builtinRules } from "eslint/use-at-your-own-risk";
import tseslint from "typescript-eslint";
import { sizeConfig } from "../packages/shared/eslint/size.mjs";
import baseline from "./eslint-baseline.json" with { type: "json" };

export default sizeConfig({ builtinRules, parser: tseslint.parser, baseline });
