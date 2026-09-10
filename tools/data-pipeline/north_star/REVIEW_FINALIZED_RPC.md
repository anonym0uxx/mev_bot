# Independent review: finalized RPC corroboration (first 10 transactions)

## Verdict

**PASS for narrowly scoped same-provider finalized-RPC corroboration, with provenance and representation limitations below. Not training admission.** Offline re-comparison of the original corrected raw capture and stored RPC bodies passed 200 explicit checks across 10 distinct transactions. The result is not inferred from the receipt's `matched` booleans. All 11 response files match their receipt SHA-256 and byte lengths; the original compressed part and all 10 exact NDJSON line hashes match.

The stored `statuses.json` contains 10 non-null `confirmationStatus: "finalized"` entries and `confirmations: null`; all status slots, transaction-result slots and raw slots equal **445762805**. Status response context slot is **445765451**. Eight executions succeeded and two failed; finalized does not mean successful. There is actual stored RPC finality evidence, not merely structural finality references, but it is a Helius assertion, not an independently verified validator/consensus proof.

## Scope and procedure

- Read-only, offline Python review; no network, new capture, RPC, orders, source edits, admission changes or training. Only this review document is written.
- Worktree: `D:/repos/mev_bot-north-star`.
- RPC directory: `D:/mev_bot-artifacts/north_star/aggregation/finalized_rpc_first10_v1`.
- Original source: `D:/mev_bot-artifacts/north_star/development/corrected_capture_validation_v1/pumpfun_laserstream_raw_v1_20260910_015052_000575_part0000.ndjson.zst`.
- Recomputed compressed source SHA-256: `034fd642be1cf852b422055199e1b630dd506e54ca0d1eb4e9a751f776543aba`.
- Recomputed `RECEIPT.json` SHA-256: `16139513fdd394a7a266a8d65722160aad384e739506b101aa11c598a71111af` (a review fingerprint, not an externally authenticated receipt signature).
- Stream-decompressed the first 100 physical NDJSON records, selected the first 10 with `record_type == "transaction"` in source order, then compared full original payloads to the saved RPC bodies. Source-line SHA-256 includes the line terminator. No projection or old reconciliation output was used as a substitute for raw balances.
- Matched full signature lists, primary signatures, selected record identities, slots, strict integer fees, success/failure, both complete native-balance arrays, all ordered static/writable/readonly keys, address-table lookup keys and their exact ordered lookup indices. Compared every token entry using exact transaction `account_index`, resolved account key, mint, owner, token program, atomic amount string, integer decimals and UI amount string; separately checked floating UI representations and recorded discrepancies. No float arithmetic was used for balances.
- Every native position is aligned to `static accountKeys + loadedAddresses.writable + loadedAddresses.readonly` in that order, never by owner/mint alone. Both native arrays contain 331 positions across the sample; all 167 static and 164 loaded key positions match (56 writable, 108 readonly). All 55 pre-token and 57 post-token entries match on exact fields. No duplicate or out-of-range token indices were found; no missing compared fields or cross-source entry omissions were found.

## Response-file integrity

All rows below matched the receipt independently.

| File | Bytes | Recomputed SHA-256 |
|---|---:|---|
| `statuses.json` | 1270 | `07eccce411a50c2d54dac58b6a5cf67b5c7ee451404faf60a233a3281f832a56` |
| `transaction_00.json` | 6223 | `93b2927f2f279f5990b2b4fab5c1c53a489006ec0bee2c76eb51aa9b71929e0b` |
| `transaction_01.json` | 10046 | `793771c68d736ffab32529201383c6d81bf96a590d9bf425d7a5fc337ff447d9` |
| `transaction_02.json` | 10038 | `6e52df5e2815561de6657e69cfd344322b87fcca3b6b4442f9194b7ce07de86e` |
| `transaction_03.json` | 22193 | `39c58f641f02471da0ea01132142d183f137232149a1cf42d918f0bbeb49cea5` |
| `transaction_04.json` | 13525 | `863502255be9ec264f62c5d6173afafdb58a51a156f62e37f983f98e3a44ab0d` |
| `transaction_05.json` | 21842 | `54f9145b9b6d07781daa371ac001b9f6b4a6e5c1fb335f4d929ccff2a52233d6` |
| `transaction_06.json` | 11763 | `cc72264b36b54ce99f8d7e7087e9bbc9f5cabd9fc0ee76912b031bc5375dd135` |
| `transaction_07.json` | 7294 | `16be0f0340c90d2029b375f438b3e4c23381b2d41c4d93aa44a35b9fa2eb41d0` |
| `transaction_08.json` | 11757 | `f3cf0ed7dc9770e2b7c7a371e992583eb643a8ad5978b97965edcab7a2d1ca8a` |
| `transaction_09.json` | 7295 | `1023d20b9207564ffe63abbcea8410682301e39ffd56a91890c986c03b2567d2` |

## Transaction results

All rows: raw/RPC signatures, fee, both native arrays, exact token fields and full ordered keys match; status is finalized at slot 445762805. `S/W/R` counts static/loaded-writable/loaded-readonly keys. File numbering and status-array indices are zero-based. Source lines are one-based.

| RPC file | Raw record / line | Fee (lamports) | Execution | S/W/R | Pre/post token entries |
|---|---|---:|---|---|---|
| `transaction_00.json` | 5 / 6 | 125001 | success | 19/0/0 | 2/2 |
| `transaction_01.json` | 7 / 8 | 1005000 | success | 14/4/9 | 1/2 |
| `transaction_02.json` | 9 / 10 | 1005000 | success | 14/4/9 | 1/2 |
| `transaction_03.json` | 11 / 12 | 704455 | success | 10/14/24 | 13/13 |
| `transaction_04.json` | 13 / 14 | 1005000 | success | 19/4/12 | 7/7 |
| `transaction_05.json` | 16 / 17 | 1366180 | success | 15/10/14 | 11/11 |
| `transaction_06.json` | 18 / 19 | 1005000 | success | 19/5/10 | 5/5 |
| `transaction_07.json` | 19 / 20 | 1005000 | failed | 19/5/10 | 5/5 |
| `transaction_08.json` | 21 / 22 | 1005000 | success | 19/5/10 | 5/5 |
| `transaction_09.json` | 24 / 25 | 1005000 | failed | 19/5/10 | 5/5 |

### Exact identity and source-line binding

- Record **5**, `transaction_00.json`: signature `5K6GJpBkjj2GsS4FyJ4Pvv5CYduHVttkAoc1jveiUxcPrJv6xaLixcMLLu9ZqG9McDPgYfTaGrrfrr37LGCMCUhU`; exact source-line SHA-256 `a1c6d6ff2a8052dd7f4e596479df63bdc604dbfdc227739d3d96a1e557955739`.
- Record **7**, `transaction_01.json`: signature `2aij3MZRfcBRADXosNgdorerGckTsha4BdHWwpXd986G5hghPaP2qkgoUpAesAiU9jkt9VnwpJhVfKFHk9SEDW61`; exact source-line SHA-256 `6eebde9e09e49d1c2dd355f811514749bdef2f48c517f66fef2a9df8f3e07f4e`.
- Record **9**, `transaction_02.json`: signature `2wTJ3tf4a3bjE4qnEhRqRZU3jCtCePMt11F3Xqg9DUBgYay82ak4HimQ5dfkBuFrnJuceSCVqiDSaCNu4vcVH5xZ`; exact source-line SHA-256 `0670a60868bc6c4d8e9cec794a2eae7586908d68f62e1aee3b6294f51910ff18`.
- Record **11**, `transaction_03.json`: signature `3ExbR2jnKecnhvNdgqtUr7RjoU1Rb38H72yBbQCKDV3LShTK6NuoM3EG4d6fWMfjr8fwWt3WwT6UKQc5DUYeVR96`; exact source-line SHA-256 `252d7ef320aa36776c899be056a943ad92e8738973e1cb15252a466ea6b6585b`.
- Record **13**, `transaction_04.json`: signature `5e9ScXjw6Wbxv2sNmRCokTaDTdcHZy2UGnTwpjdagZUKQFEDm1ydyKtdozLXz8jrKxgRcbrnFAfWW5oXvwBhnrLJ`; exact source-line SHA-256 `8fff6ae679bc33c646727d2d72ce9c386f7b8d990b0a490f0c59567caa2878e5`.
- Record **16**, `transaction_05.json`: signature `3JNmpxsFSPdJTk53GiJNTG1CyvcrGWH62qXvHZLrwr1QkR5hcMH6HGT35TRaEKsE6rZDSXtSgWjYFxj49R6vrpLC`; exact source-line SHA-256 `b7c6b6e55989701ee0e8450b3eba7c30ae8cb29a9f857c52b3f054464ccd7127`.
- Record **18**, `transaction_06.json`: signature `tUdNuVXdCk1ku5NoamGhJWwq8FZLWtPpwfyyoiKJbEbDxBdd2ygnakGgiPdDNgrASzA3oX9FagdMymNFhRWtkrK`; exact source-line SHA-256 `4b782a5a0b4bfecc9cf8cf752c79c4f43c7c4941e65fec072df263e974d0ad8a`.
- Record **19**, `transaction_07.json`: signature `3iQrVGPGu4GWp7rFGi7istkJTwYqmuDgHJWnBF9wmQeV6umrC1wAJ6NcoSQHTz2Sbhs7yXkdnkr2DMe3eigYxouW`; exact source-line SHA-256 `d76eca5398ab7ca096a282506c080d55521f71d5290cafd77de15959e7ff3f0b`.
- Record **21**, `transaction_08.json`: signature `4qfWwFQfB75hX73nLotbMdZpKnauVYCWqVgFGDvtaVm6X7qZBwWhvbY2ec3ruRVWEV7RAMUdPRYHuvQ2CtKQWnAn`; exact source-line SHA-256 `a1587e80e66bbb23c453ac2971c663502a4aa86522b5af4680d6f7e800fa0960`.
- Record **24**, `transaction_09.json`: signature `2VDFDDYYvtk3P7KB6hv8j13citbDVniwNjzx7imp5eFdaTuBGB9FFrGTwMjNy68XnWY38mUPDhtjohVpSySqX1vo`; exact source-line SHA-256 `7479b60e6e2077b2aee57be8df84d760693d22e96f7af1fa32e7cb9797b6e075`.

### Full-vector comparison fingerprints

These fingerprints are review-computed SHA-256 over UTF-8 `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`; full vectors were directly compared before hashing. `keys` is the ordered concatenation, not a sorted set. Each vector fingerprint applies equally to raw and RPC values.

| Record | Pre-native SHA-256 | Post-native SHA-256 | Ordered full-key SHA-256 | Loaded writable / readonly transaction-index ranges |
|---|---|---|---|---|
| 5 | `f6141aa851da03f11f2d12e242219ceeb59405aa431fd54c9ade0536207eecb4` | `262ba5630b05c8b02d5ad2761f6a52c2f45ff2cfb6f174f781b8b4d964581fe7` | `24bb88716391852158a76fbb60604638d6b01fb637e68d9a17d936ef4eb941e1` | empty / empty |
| 7 | `6a9d05fe0919cc325529c5a5df86676df3f311b609afe4dca49025a1f9d16861` | `7e305a84ce1465e417619caae1b310081f0ca90d8f9b0da646a5249ad6f46db8` | `df534419d513841de0b7cd867284dbc75cbcaf1adb2227a974b546fe6848bcd8` | 14..17 / 18..26 |
| 9 | `465b78f82eb911601338213effd966d290fba355578e32e4c6c341efbd31d2d4` | `d25e50e81ff9896854e827e1d87f94b2caec987475e8737b2561e088d5afd1a6` | `b6a6677acb874043f20a4a6e565599022c93687b4831d128274f9517b5e0a63f` | 14..17 / 18..26 |
| 11 | `f44eb056eab4f338feeebeb073f07db9d2e83a0b00642be37e1624332195696f` | `292fb462e697f2cc09547d467b73cfaffd54a40449737ef18cf0c338182f7360` | `c634d53e0cc1f598875cf385f769f380090f818bc1a5f854ab70ed274f5b2736` | 10..23 / 24..47 |
| 13 | `9f3cb64ab927196b15b1e807d9c79fcd9753239ad2ddc1f84738a1f51158c8c5` | `c53ecc6637f2969addfa12e298d0d548e2b6a3f5eacae8f79e606051c48a03fb` | `99bdf65c69fdf742572dddccb277b6fd0796c394dea860e8090a12a5a756eaa2` | 19..22 / 23..34 |
| 16 | `10ac842cd10da25fde0dfdc373fd1ad8df5dd2031c9ea99a2900a57558562902` | `5c3ed1acee244052388b3cff4fd03c3fbf8ef0834c36a7ed90638eddbaf77351` | `280823eb6395f7364b2502c23496add37439d4ffed7f25347badd9261ff336d3` | 15..24 / 25..38 |
| 18 | `19c58f6603fda48d4fbddda6e1aef9e2bdc07c35e055154ec71f93115558ef8d` | `95c7024ab05a545c21afbaa1d9ee7c65e97f68b4eb0ea085fd26df323f4188c6` | `b5a15b83c8adfd286e2982b519ffc2edd3d714b344812c170e6432377a79d1a7` | 19..23 / 24..33 |
| 19 | `d9719bb8896deee1aa5fe5f5e9c68b0f787c3579afab047409424632e6e04b44` | `78e7f4f4ab88e10df50a75787d7998f3ad965c1d8dafdf3f7d3e08adb64c919d` | `61e8a7f60cd623e3aab4c731788b977e748959f9b49ee882ad4b5d25d5fc212f` | 19..23 / 24..33 |
| 21 | `6366e50856d7600796b7a17445bf54a9187ff9346f6058e9c91fbd9fd021e3cd` | `929e3348b317755d49deaf26f039a51d2ef9d849c0e8a65aca6c5ce23a708405` | `1496be02b499251d4b0067c83c5ce9f5c2fe6f68d75b0edb4e39c67763a96b21` | 19..23 / 24..33 |
| 24 | `baf6a36f085f13b7ab74adfd64ea898eca03f1737a1c25b0da6d938118b0f0a5` | `e7e086ae347c2bdaf38b3cf3b5fe3047c64f5ca3acbe314496d87ec951dc606f` | `a215e84dcf6f57049045b0e371f4e84c575924a59e33165713c2fd83e454a5e8` | 19..23 / 24..33 |

## Exact token-account endpoints

Atomic quantities below match raw and RPC independently at the displayed **transaction account index and resolved key**. Mint, owner, program ID, decimals and UI string were also checked for every pre/post entry. `ABSENT` is absent in both source and RPC, **not zero**. Displaying adjacent endpoints is not asserting a fill, ownership continuity, profit, or training eligibility.

| Record | Account index | Resolved account key | Pre atomic amount | Post atomic amount | Decimals |
|---|---:|---|---:|---:|---:|
| 5 | 4 | `5Sa36wageVvE95eseA9aCrzwv6Th2FRMCgWLtRw5N1De` | 493953916719533 | 549698163382997 | 6 |
| 5 | 5 | `GRXgK1QgJimSG5pFPK4pc3fh4qeXtmk4RLCzaWg65AGs` | 55744246663464 | 0 | 6 |
| 7 | 2 | `9TRyxj7p7CYpC6P2jaeqYey2jnPXKNsNeACbDrX3rEd8` | ABSENT | 37518437650448 | 6 |
| 7 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 638327122757068 | 600808685106620 | 6 |
| 9 | 2 | `5qD9LsoPtXpLnFLG2Lb6Aot7Ge1SqvfzRfrq1D2oPnKh` | ABSENT | 31836122971532 | 6 |
| 9 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 600808685106620 | 568972562135088 | 6 |
| 11 | 2 | `GVcFNiafsU9HEkJFeNpAZJgreU5FmeWc5jUP95map9x6` | 3398118633903 | 0 | 6 |
| 11 | 4 | `4VHc5TgbyFMX3YMsSAP8wLZSWA6zkg6mMP67sBjqURKx` | 966561674 | 966941674 | 6 |
| 11 | 10 | `Bvtgim23rfocUzxVX9j9QFxTbBnH8JZxnaGLCEkXvjKS` | 1621849153235 | 1621849546518 | 9 |
| 11 | 11 | `GAFuhgcd328SkkBYHpfadzmef9hTGAFRCi9QoCnsZQug` | 500217361 | 500610643 | 9 |
| 11 | 13 | `FKsiKLFKpEa4aG8rMc8LJzkx2VBoT9fpxvmwhXcMRoEx` | 195097190035950 | 198495308669853 | 6 |
| 11 | 14 | `2EGow3xVV1NuW7PphKugBT1aB52e689GeF11JCgaPbkd` | 74307092091 | 72737110238 | 9 |
| 11 | 15 | `9HkQ8uKP4RLdBw9eQh8iNkLGHQAdtkYqbJPvUBhMken7` | 9294972240 | 9309916958 | 9 |
| 11 | 17 | `8BrVfsvzb1DZqCactbYWoKSv24AfsLBuXJqzpzYCwznF` | 301608800849 | 303163051419 | 9 |
| 11 | 18 | `HsQcHFFNUVTp3MWrXYbuZchBNd4Pwk8636bKzLvpfYNR` | 65796586713 | 65639768561 | 6 |
| 11 | 20 | `Cv9St5tDTGwpbG5UVvM6QvFmf3FYSXc14W9BYvQN5wAZ` | 298055292073 | 298055292073 | 9 |
| 11 | 21 | `7Rf8Gu8YemSoGjZT3z1cL5BT9HLbGywcyaz8Mrbhd1MH` | 72022932143 | 72022932143 | 6 |
| 11 | 22 | `4aShazcPQGVp5K1F3pT5hZyv4GeMJF3YTzFJsJzqvnLY` | 180000 | 156048152 | 6 |
| 11 | 23 | `HrTf9CzXR1dRH4Sof5QrpmGWwpwAf3qZzwCsEjQpXcSq` | 865173621766 | 865174191766 | 6 |
| 13 | 3 | `cv3xakMX1wteaSTaXUecck5gtHCNjt3S7cULNdBxGgV` | 5760667346308 | 4608533877047 | 6 |
| 13 | 6 | `9sbewmWS9n51N94T3EdbWZeAUcB1yRXo6rRZGHft96b5` | 61020374717434 | 62172508186695 | 6 |
| 13 | 7 | `FnnC1r8CHrvtb3MWmMPUwZRUZk4ipKW8JWsm1aosAFm3` | 298355218539 | 292512174220 | 9 |
| 13 | 8 | `4WMDEx1RyiQR56Xhe56oikLJQD6YYxiioz15E8EKmmo3` | 0 | 0 | 9 |
| 13 | 9 | `7aJCMzrcyR3y98fjyZscv8JkmscU9dtDvXPZNs9hfuzR` | 517218062 | 561128716 | 9 |
| 13 | 20 | `X5QPJcpph4mBAJDzc4hRziFftSbcygV59kRb2Fu6Je1` | 2873471310724 | 2873472774413 | 9 |
| 13 | 21 | `GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd` | 747238816 | 748702504 | 9 |
| 16 | 4 | `7HtqHtPbeNQKMW8CfPzmY6myHtbsdb9EfDtYr4iL2Uxn` | 0 | 0 | 6 |
| 16 | 5 | `7NqJk39tWVboE6KGp4tjALmS8WK785c5cieNn1qcJ5ee` | 15724004 | 15724428 | 6 |
| 16 | 7 | `DX5B9DYEDDeamt3hoTX4zp7c7PGA6sKx87d144vSWDhF` | 16552819 | 21599137 | 9 |
| 16 | 8 | `G7KTwHWAsb9w7c4JvN3W7VLyvTh72jiQnWjYEKC2hs7d` | 2726399862854 | 2964551737743 | 6 |
| 16 | 9 | `HdeTxVpFmwZLeuhgk6K9xFCu81LG7mKRiLQdyYsegueY` | 0 | 0 | 9 |
| 16 | 15 | `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb` | 6538250937797 | 6538251077973 | 9 |
| 16 | 17 | `CA7v8gHfbquYXyDnDx6QxWW8hmL1H7X6Y2RYDrGLnuck` | 558778936 | 558919111 | 9 |
| 16 | 20 | `DT2krA8vSP96D68nH86eAztZHVM18YB5i4gDXjLBDXm7` | 4333481879 | 4333481879 | 9 |
| 16 | 21 | `5n2YQGBuxUEE5pgyRjJtsfWK1ArmB7hZQK9Nq9zHNSEw` | 197761086534 | 198322909865 | 9 |
| 16 | 23 | `AveR6GiPTLzSZuiNU4jyBPfWGbARGBcvdBRQd1Cia4ZC` | 91703784803657 | 91465632928344 | 6 |
| 16 | 24 | `EGVWGGyktoLR1pnVvLs6dNdmUb24VeM2fNKoggwCQZgt` | 7007896 | 7007896 | 9 |
| 18 | 4 | `8GyRSm6MjWV3xw58MKfF3YLbmSKVUv3K96cR86hThToZ` | 1933331127561 | 0 | 6 |
| 18 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 568972562135088 | 570905893262649 | 6 |
| 18 | 8 | `AG73KqS4DjFTaeKgTc7pm9EsnfEaSxamdhAMsXwBSwan` | 0 | 0 | 9 |
| 18 | 20 | `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb` | 6538251077973 | 6538251077973 | 9 |
| 18 | 22 | `6rVkF4HSgy1jrnC3HogfRgPHrq4CtLg5f11URpsC4i9D` | 522825736 | 522825736 | 9 |
| 19 | 4 | `8GyRSm6MjWV3xw58MKfF3YLbmSKVUv3K96cR86hThToZ` | 0 | 0 | 6 |
| 19 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 570905893262649 | 570905893262649 | 6 |
| 19 | 8 | `AG73KqS4DjFTaeKgTc7pm9EsnfEaSxamdhAMsXwBSwan` | 0 | 0 | 9 |
| 19 | 20 | `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb` | 6538251077973 | 6538251077973 | 9 |
| 19 | 22 | `6rVkF4HSgy1jrnC3HogfRgPHrq4CtLg5f11URpsC4i9D` | 522825736 | 522825736 | 9 |
| 21 | 4 | `4ewvfzo7qqNwAnrAGtqr4Dii56S4rcrWa8oDnFoATJKz` | 447794989411 | 0 | 6 |
| 21 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 570905893262649 | 571353688252060 | 6 |
| 21 | 8 | `AG73KqS4DjFTaeKgTc7pm9EsnfEaSxamdhAMsXwBSwan` | 0 | 0 | 9 |
| 21 | 20 | `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb` | 6538251077973 | 6538251077973 | 9 |
| 21 | 22 | `GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd` | 748702504 | 748702504 | 9 |
| 24 | 4 | `4ewvfzo7qqNwAnrAGtqr4Dii56S4rcrWa8oDnFoATJKz` | 0 | 0 | 6 |
| 24 | 5 | `A1VTztYihNU5Am1gVDiSMvivmmS3gyJVWJqg7vtbgSqc` | 571353688252060 | 571353688252060 | 6 |
| 24 | 8 | `AG73KqS4DjFTaeKgTc7pm9EsnfEaSxamdhAMsXwBSwan` | 0 | 0 | 9 |
| 24 | 20 | `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb` | 6538251077973 | 6538251077973 | 9 |
| 24 | 22 | `GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd` | 748702504 | 748702504 | 9 |

## Explicit discrepancies, omissions and limits

### Token display representation differs (22 entries)

Full JSON token objects are **not identical**: raw `ui_amount` is `0.0` while RPC `uiAmount` is `null` in the following entries. In every such entry, atomic `amount` is exactly `"0"`, decimals match, and both UI amount strings are `"0"`. All other compared UI numeric values agree. This is a representation discrepancy, not an atomic-balance mismatch; it was not silently normalized into an exact-object pass.

| Record | Side | Exact account indices |
|---|---|---|
| 5 | post | 5 |
| 11 | post | 2 |
| 13 | pre | 8 |
| 13 | post | 8 |
| 16 | pre | 4, 9 |
| 16 | post | 4, 9 |
| 18 | pre | 8 |
| 18 | post | 4, 8 |
| 19 | pre | 4, 8 |
| 19 | post | 4, 8 |
| 21 | pre | 8 |
| 21 | post | 4, 8 |
| 24 | pre | 4, 8 |
| 24 | post | 4, 8 |

### Missing pre-token counterparts remain

- Record **7**, account index **2**, key `9TRyxj7p7CYpC6P2jaeqYey2jnPXKNsNeACbDrX3rEd8`: pre-token entry absent in both raw and finalized RPC; post-token entry present in both. No zero or opening inventory is inferred.
- Record **9**, account index **2**, key `5qD9LsoPtXpLnFLG2Lb6Aot7Ge1SqvfzRfrq1D2oPnKh`: pre-token entry absent in both raw and finalized RPC; post-token entry present in both. No zero or opening inventory is inferred.

`FIRST10_PARENT_RECONCILIATION_V2.json` was separately inspected: 8 reconciled records and 2 `missing_token_counterpart` rejections (records 7 and 9). Finality corroboration does not repair either missing pre-token endpoint or change that 8/2 result. There are no pre-only/post-missing token entries in this sample.

### Finalized failures are not fills

Records **19 and 24** are finalized failures. Both RPC transaction metadata and signature-status entries agree exactly on `{"InstructionError":[2,{"Custom":6000}]}`. Raw `err_is_none` is false and `err_hex` is `08000000021900000070170000` for both. Raw success/failure was verified; the raw serialized error bytes were not decoded against a separately version-pinned Solana error schema, so exact raw-error semantic decoding is not claimed. No successful trade is inferred from finality.

### Provenance and finality strength

1. **Same provider, not independent validator.** Receipt identifies Helius; raw LaserStream and later RPC are same-provider evidence. This is an independent offline comparison by the reviewer, not independent provider, validator, consensus replay, block-inclusion proof or signature-cryptographic verification. No network call was made by this reviewer. Saved response hashes establish consistency with the local receipt, not external authenticity or historical immutability.
2. **Request envelopes are not retained in this directory.** The 12 files are receipt plus 11 responses; no saved request bodies, signature-array request, HTTP transport log or authenticated provider identity accompany them. Receipt records method, response path, timestamps, lengths and hashes, but does not retain `searchTransactionHistory`, `commitment`, `encoding`, or `maxSupportedTransactionVersion` parameters. Parent reports `getSignatureStatuses` with history and `getTransaction` with `commitment=finalized`, `encoding=json`, `maxSupportedTransactionVersion=0`; those exact options cannot be independently recovered from these response bodies.
3. **Status-to-signature binding is positional/provenance-dependent.** `getSignatureStatuses.result.value` entries contain no signature. Mapping entry i to `transaction_ii.json` and the ith selected signature relies on the receipt/parent request ordering, independently checked for internal consistency. Transaction responses themselves embed the exact raw signatures; all status slots and success/error results agree, but this is not a self-authenticating signature binding. Most successful entries share the same slot/status, so reordering those status entries is not detectable from the body alone.
4. **Direct finality declaration comes from status bodies.** `getTransaction` results contain matching transaction data but no `confirmationStatus` or echoed requested commitment. Do not claim the transaction body alone proves a finalized request; finality support here is the explicit stored finalized statuses plus documented call provenance.
5. **Later evidence cannot become an earlier feature.** Selected raw receive times are 2026-09-10T01:50:52.530000+00:00 through 2026-09-10T01:50:52.559000+00:00. Receipt says status request ran 2026-09-10T02:04:44.870592+00:00 to 2026-09-10T02:04:45.057831+00:00, and transaction calls completed by 2026-09-10T02:04:47.430824+00:00. Finality is later corroboration; do not backdate availability, rewrite earlier feature timestamps or infer exact time the slot first finalized.
6. **Limited sample and accounting scope.** Only the first 10 transactions within the first 100 records were compared. No whole-capture completeness, provider independence, venue fill decoding, owner-history continuity, opening basis, PnL, admission or training completion follows. Lookup descriptors and loaded addresses agree between records; historical address-table account contents were not independently fetched or reconstructed. Inner instructions, logs, return data, rewards and compute accounting were not substantively reviewed here.

## Disposition

Accept these stored files as hash-bound, same-provider, later finalized-RPC corroboration of the ten selected raw transactions under the stated positional-provenance limitation. Preserve all original files and existing quarantine/admission decisions. Receipt `semantic_trade_admission=false`, `historical_available_at_override=false`, and per-comparison `training_eligible=false` remain unchanged. No source or admission file was modified.
