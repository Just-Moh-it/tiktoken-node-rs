import { SpecialTokenAction, SupportedEncoding, getTokenizer } from "./index";

const tokenizer = getTokenizer();

console.log(
  "Tokens",
  await tokenizer.exactNumTokens(
    "Sydney",
    SupportedEncoding.Cl100k,
    SpecialTokenAction.NormalText,
    {}
  )
);
