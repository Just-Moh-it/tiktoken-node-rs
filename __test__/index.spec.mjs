import test from 'ava'
import { SpecialTokenAction, SupportedEncoding, getTokenizer } from '../index.js'

const tokenizer = getTokenizer();

test('exactNumTokens with Sydney', async (t) => {
  const tokens = await tokenizer.exactNumTokens(
    "Sydney",
    SupportedEncoding.Cl100k,
    SpecialTokenAction.NormalText,
    {}
  )
  t.is(tokens, 2)
})
