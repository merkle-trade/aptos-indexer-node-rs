/* tslint:disable */
/* eslint-disable */

/* auto-generated by NAPI-RS */

export interface TransactionFilter {
  focusContractAddresses: Array<string>
}
export function startFetchTransactions(url: string, authKey: string | undefined | null, startVersion: bigint, endVersion?: bigint | undefined | null, filter?: TransactionFilter | undefined | null): Promise<number>
export function nextTransactions(ch: number): Promise<string | null>
export function sum(a: number, b: number): number
