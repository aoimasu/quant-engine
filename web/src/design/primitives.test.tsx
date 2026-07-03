import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Input } from './Input';
import { Select } from './Select';
import { DataTable } from './DataTable';
import type { Column } from './DataTable';
import { Tag } from './Tag';
import { Pnl } from './Pnl';
import { Tabs } from './Tabs';

describe('ported design primitives (QE-259)', () => {
  it('Input renders a labelled field wired to its control', () => {
    render(<Input label="Taker fee (bps)" defaultValue="2.0" />);
    const input = screen.getByLabelText('Taker fee (bps)');
    expect(input).toHaveValue('2.0');
  });

  it('Select renders its options and reflects the value', () => {
    render(<Select aria-label="Resolution" value="1h" onChange={() => {}} options={['1m', '1h', '1d']} />);
    const select = screen.getByLabelText('Resolution');
    expect(select).toHaveValue('1h');
    expect(screen.getByRole('option', { name: '1d' })).toBeInTheDocument();
  });

  it('DataTable renders rows and fires onRowClick', async () => {
    type Row = { id: string; sym: string } & Record<string, unknown>;
    const cols: Column<Row>[] = [
      { key: 'id', header: 'ID' },
      { key: 'sym', header: 'Symbol' },
    ];
    const rows: Row[] = [{ id: 'a', sym: 'BTC' }];
    const onRowClick = vi.fn();
    const { container } = render(
      <DataTable columns={cols} rows={rows} keyField="id" onRowClick={onRowClick} />,
    );
    expect(container.querySelector('table.qe-table')).not.toBeNull();
    expect(screen.getByText('BTC')).toBeInTheDocument();
    await userEvent.click(screen.getByText('BTC'));
    expect(onRowClick).toHaveBeenCalledOnce();
  });

  it('Tag renders with the design class and mono modifier', () => {
    render(<Tag mono>lookback=48h</Tag>);
    expect(screen.getByText('lookback=48h')).toHaveClass('qe-tag', 'qe-tag--mono');
  });

  it('Pnl colour-codes a positive percent', () => {
    render(<Pnl value={3.23} format="percent" />);
    const el = screen.getByText('+3.23%');
    expect(el).toHaveClass('qe-pnl', 'qe-pnl--up');
  });

  it('Tabs marks the active tab and fires onChange', async () => {
    const onChange = vi.fn();
    render(
      <Tabs
        tabs={[
          { value: 'overview', label: 'Overview' },
          { value: 'trades', label: 'Trades' },
        ]}
        value="overview"
        onChange={onChange}
      />,
    );
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveAttribute('aria-selected', 'true');
    await userEvent.click(screen.getByRole('tab', { name: 'Trades' }));
    expect(onChange).toHaveBeenCalledWith('trades');
  });
});
