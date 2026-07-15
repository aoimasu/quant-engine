import { describe, it, expect } from 'vitest';
import { DataTable } from './DataTable';
import type { Column } from './DataTable';

/*
 * QE-423 acceptance criterion, encoded as a type-level test: `Column<Row>.key` is constrained to
 * `keyof Row & string`, so a column referencing a NON-EXISTENT key is a COMPILE error (no longer a
 * silent `undefined` cell). These assertions are exercised by `tsc -b` (`npm run build`) because
 * `tsconfig.app.json` type-checks all of `src` — if the constraint regresses, the `@ts-expect-error`
 * directives below go unused and `tsc` fails the build. The runtime `it` only exists so vitest counts
 * this file; the real guards are the type directives.
 */

interface Row {
  id: string;
  count: number;
}

// Real keys compile, AND `render`'s value is typed to `Row[key]` (string for `id`, number for `count`).
const ok: Column<Row>[] = [
  { key: 'id', header: 'ID', render: (v) => v.toUpperCase() },
  { key: 'count', header: 'N', align: 'num', render: (v) => v.toFixed(2) },
  // A derived column identified by `id` (no backing `Row` key) is allowed for computed cells.
  { id: 'derived', header: 'D', render: (_v, row) => `${row.id}:${row.count}` },
];

// @ts-expect-error — 'nope' is not a key of Row (AC: a non-existent key is a compile error).
const badKey: Column<Row>[] = [{ key: 'nope', header: 'X' }];

// @ts-expect-error — `id`'s cell value is `string`; calling a number method on it is a type error.
const badValue: Column<Row>[] = [{ key: 'id', header: 'X', render: (v) => v.toFixed(2) }];

describe('Column<Row> generic key typing (QE-423)', () => {
  it('constrains keys and cell-value types at compile time (see @ts-expect-error above)', () => {
    // Type-level guards above are the assertion; touch the values so `noUnusedLocals` is satisfied and
    // the file registers as an executed test.
    expect(ok).toHaveLength(3);
    expect(badKey).toHaveLength(1);
    expect(badValue).toHaveLength(1);
    expect(DataTable).toBeTypeOf('function');
  });
});
