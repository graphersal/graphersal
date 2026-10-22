// Syntax highlighting for GQL text (the saved-query editor's GQL mode) as a CodeMirror 6
// StreamLanguage over the pure tokenizer js/editor/gql-tokens.js. Colours: the DSL's highlight
// style (js/editor/dsl-language.js), so light and dark themes need nothing of their own.

import { StreamLanguage } from '@codemirror/language';
import { scanGql, gqlStartState, gqlStartLine } from './gql-tokens.js';

/** The CodeMirror token name of a tokenizer type (styled by `dslHighlight`). */
const STYLE = {
  comment: 'comment', string: 'string', number: 'number', keyword: 'keyword', atom: 'atom',
  param: 'variableName.special', label: 'typeName', property: 'propertyName', name: 'variableName',
  function: 'propertyName', procedure: 'propertyName', variable: 'variableName', operator: 'operator', punctuation: 'punctuation',
};

export const gqlLanguage = StreamLanguage.define({
  name: 'gql',
  startState: gqlStartState,
  copyState: (state) => ({ ...state }),
  token(stream, state) {
    if (stream.sol()) gqlStartLine(state);
    const { end, type } = scanGql(stream.string, stream.pos, state);
    stream.pos = Math.max(end, stream.pos + 1);
    return type ? STYLE[type] : null;
  },
  languageData: {
    commentTokens: { line: '//', block: { open: '/*', close: '*/' } },
    closeBrackets: { brackets: ['(', '[', '{', "'", '"'] },
  },
});
