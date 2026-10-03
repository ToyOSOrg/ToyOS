---
status: open
kind: tooling
opened: 2026-09-29
---

# Linux's readings of the T14 and the TCG model lack reads owed before the T14's wipe

The floor of
`issues/kernel/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`
is tag `Ubuntu-6.8.0-142.142` (53e5d07aac028a1523ab0b115f079d6d1bc831ef) of
`https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
config sha256 3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890,
and what it reads on the T14 and on the TCG model are the fixtures
of `issues/kernel/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`,
`toyos-cpuvuln/fixtures/`'s `t14.txt`, `t14/` and `tcg/`;
`the_t14s_facts_are_linuxs_reading_of_it` and
`the_tcg_models_signature_gives_its_lines` hold `T14` and `TCG` to them.
Ubuntu leaves the T14 only after this issue and
`issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`
close.

**Exit**: before the wipe the T14's readings add CPUID 5, 0x19 and
0x80000001, MSR 0xCF, MSR 0x3A (`rdmsr -a 0x3a`, IA32_FEAT_CTL), the
split-lock line, and the config's `X86_KERNEL_IBT` and
`X86_INTEL_MEMORY_PROTECTION_KEYS`, each committed with the test that reads
it, and `the_t14s_facts_are_linuxs_reading_of_it` holds `T14`'s `feat_ctl`
to 0x3A's. **Mutation**: an added reading off by one bit reds the test that
reads it. **Oracle**: that Linux.

## The counters' oracle, read 2026-10-03

The T14's hardware counters (`issues/diagnostics/toyos-explains-itself.md`)
have no independent oracle but this Linux. Run as root under Ubuntu's
`6.8.0-142-generic` from 16:22:37 UTC, one after the other; each output below
is whole but for trailing blanks, and none was read by a test yet: the
counters' host test reads them when it lands.

Idle, 60 s: `turbostat --quiet --interval 10 --num_iterations 6 --show
Core,CPU,Avg_MHz,Busy%,Bzy_MHz,TSC_MHz,IRQ,SMI,CPU%c1,CPU%c6,CPU%c7,CoreTmp,PkgTmp,Pkg%pc2,Pkg%pc3,Pkg%pc6,Pkg%pc7,Pkg%pc8,Pkg%pc9,Pk%pc10,PkgWatt,CorWatt`.
Machine-wide per 10 s: busy 0.11 to 0.36%, busy clock 980 to 1668 MHz, TSC
2419 MHz, SMI 0, core C7 98.78 to 100.08%, package PC8 75.47 to 82.10%,
package 1.14 to 1.88 W, cores 0.07 to 0.13 W.

```
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	1	0.11	994	2419	1160	0	0.47	0.00	99.98	29	31	17.46	2.01	0.03	0.29	77.22	0.00	0.00	1.30	0.07
0	0	1	0.11	1012	2419	131	0	0.12	0.00	100.42	26	31	17.46	2.01	0.03	0.29	77.22	0.00	0.00	1.30	0.07
0	4	0	0.05	910	2419	53	0	0.12
1	1	1	0.08	986	2419	57	0	0.30	0.00	100.18	27
1	5	1	0.12	999	2419	119	0	0.30
2	2	1	0.13	933	2419	423	0	1.29	0.00	99.04	29
2	6	2	0.19	1002	2419	157	0	1.29
3	3	1	0.14	978	2419	148	0	0.16	0.00	100.27	28
3	7	1	0.09	1115	2419	72	0	0.16
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	1	0.11	980	2419	1009	0	0.38	0.00	100.08	29	30	19.01	1.80	0.05	0.22	76.10	0.00	0.00	1.81	0.07
0	0	1	0.07	975	2419	46	0	0.14	0.00	100.37	26	30	19.01	1.80	0.05	0.22	76.10	0.00	0.00	1.81	0.07
0	4	1	0.11	913	2419	72	0	0.14
1	1	0	0.03	997	2419	23	0	0.19	0.00	100.34	25
1	5	1	0.13	993	2419	130	0	0.19
2	2	1	0.11	943	2419	386	0	1.14	0.00	99.27	29
2	6	2	0.18	1035	2419	155	0	1.14
3	3	1	0.14	967	2419	141	0	0.06	0.00	100.36	27
3	7	1	0.09	997	2419	56	0	0.06
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	1	0.11	1005	2419	1125	0	0.42	0.00	100.03	29	31	19.47	1.35	0.06	0.23	75.87	0.00	0.00	1.43	0.07
0	0	1	0.12	1081	2419	42	0	0.08	0.00	100.39	26	31	19.47	1.35	0.06	0.23	75.87	0.00	0.00	1.43	0.07
0	4	1	0.13	998	2419	56	0	0.08
1	1	1	0.10	1006	2419	127	0	0.22	0.00	100.21	25
1	5	1	0.14	1004	2419	132	0	0.22
2	2	2	0.16	981	2419	513	0	1.33	0.00	99.12	29
2	6	1	0.08	1028	2419	75	0	1.33
3	3	1	0.13	953	2419	137	0	0.05	0.00	100.42	28
3	7	1	0.06	1019	2419	43	0	0.05
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	6	0.36	1668	2419	1966	0	1.14	0.00	98.78	29	32	18.49	1.12	0.04	0.33	75.47	0.00	0.00	1.14	0.13
0	0	7	0.43	1587	2419	231	0	0.90	0.00	99.10	26	32	18.49	1.12	0.04	0.33	75.47	0.00	0.00	1.14	0.13
0	4	2	0.15	994	2419	96	0	0.90
1	1	11	0.48	2410	2419	185	0	1.08	0.00	98.67	27
1	5	7	0.46	1450	2419	348	0	1.08
2	2	8	0.48	1564	2419	635	0	1.97	0.00	98.11	29
2	6	1	0.08	1040	2419	72	0	1.97
3	3	12	0.69	1725	2419	326	0	0.60	0.00	99.24	28
3	7	1	0.08	1053	2419	73	0	0.60
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	1	0.12	984	2419	1166	0	0.39	0.00	100.01	29	31	13.62	2.79	0.04	0.39	80.55	0.00	0.00	1.30	0.07
0	0	0	0.05	957	2419	34	0	0.09	0.00	100.41	26	31	13.62	2.79	0.04	0.39	80.55	0.00	0.00	1.30	0.07
0	4	1	0.13	943	2419	78	0	0.09
1	1	1	0.05	982	2419	47	0	0.36	0.00	100.00	27
1	5	2	0.23	1023	2419	186	0	0.36
2	2	1	0.05	1001	2419	45	0	0.09	0.00	100.38	29
2	6	2	0.15	989	2419	135	0	0.09
3	3	2	0.22	959	2419	535	0	1.04	0.00	99.26	28
3	7	1	0.10	994	2419	106	0	1.04
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc3	Pkg%pc6	Pkg%pc7	Pkg%pc8	Pkg%pc9	Pk%pc10	PkgWatt	CorWatt
-	-	1	0.12	980	2419	1156	0	0.39	0.00	100.03	28	31	13.35	1.53	0.07	0.41	82.10	0.00	0.00	1.88	0.07
0	0	1	0.05	958	2419	40	0	0.16	0.00	100.38	26	31	13.35	1.53	0.07	0.41	82.10	0.00	0.00	1.88	0.07
0	4	1	0.11	947	2419	82	0	0.16
1	1	1	0.09	989	2419	76	0	0.26	0.00	100.09	26
1	5	2	0.19	990	2419	174	0	0.26
2	2	1	0.09	965	2419	335	0	0.96	0.00	99.42	28
2	6	2	0.19	995	2419	178	0	0.96
3	3	2	0.19	976	2419	242	0	0.18	0.00	100.24	28
3	7	0	0.04	1021	2419	29	0	0.18
```

Loaded, 60 s, eight `yes > /dev/null` started 2 s before: `for i in 1 2 3 4 5
6 7 8; do timeout 65 sh -c "yes > /dev/null" & done; sleep 2; turbostat
--quiet --interval 10 --num_iterations 6 --show
Core,CPU,Avg_MHz,Busy%,Bzy_MHz,TSC_MHz,IRQ,SMI,CPU%c1,CPU%c6,CPU%c7,CoreTmp,PkgTmp,Pkg%pc2,Pkg%pc6,Pk%pc10,PkgWatt,CorWatt;
wait`. Busy 99.77%; busy clock 3800 MHz at 30.24 and 30.73 W package for the
first two intervals, 3555 MHz at 27.20 W in the third, then 3075 to 3094 MHz
at 19.62 to 19.92 W; about 10,000
interrupts per CPU per 10 s; SMI 0.

```
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3791	99.77	3800	2419	80691	0	0.00	0.00	0.00	71	71	0.00	0.00	0.00	30.24	27.88
0	0	3791	99.77	3800	2419	10023	0	0.00	0.00	0.00	61	71	0.00	0.00	0.00	30.24	27.88
0	4	3791	99.77	3800	2419	10021	0	0.00
1	1	3791	99.77	3800	2419	10066	0	0.00	0.00	0.00	71
1	5	3791	99.77	3800	2419	10102	0	0.00
2	2	3791	99.77	3800	2419	10031	0	0.00	0.00	0.00	62
2	6	3791	99.77	3800	2419	10274	0	0.00
3	3	3791	99.77	3800	2419	10125	0	0.00	0.00	0.00	71
3	7	3791	99.77	3800	2419	10049	0	0.00
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3791	99.77	3800	2419	80659	0	0.00	0.00	0.00	79	79	0.00	0.00	0.00	30.73	28.29
0	0	3791	99.77	3800	2419	10012	0	0.00	0.00	0.00	68	79	0.00	0.00	0.00	30.73	28.29
0	4	3791	99.77	3800	2419	10014	0	0.00
1	1	3791	99.77	3800	2419	10026	0	0.00	0.00	0.00	79
1	5	3791	99.77	3800	2419	10095	0	0.00
2	2	3791	99.77	3800	2419	10017	0	0.00	0.00	0.00	69
2	6	3791	99.77	3800	2419	10364	0	0.00
3	3	3791	99.77	3800	2419	10122	0	0.00	0.00	0.00	78
3	7	3791	99.77	3800	2419	10009	0	0.00
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3547	99.77	3555	2419	80555	0	0.00	0.00	0.00	66	66	0.00	0.00	0.00	27.20	24.72
0	0	3547	99.77	3555	2419	10021	0	0.00	0.00	0.00	58	66	0.00	0.00	0.00	27.20	24.72
0	4	3547	99.77	3555	2419	10010	0	0.00
1	1	3547	99.77	3555	2419	10028	0	0.00	0.00	0.00	65
1	5	3547	99.77	3555	2419	10037	0	0.00
2	2	3547	99.77	3555	2419	10028	0	0.00	0.00	0.00	60
2	6	3547	99.77	3555	2419	10246	0	0.00
3	3	3547	99.77	3555	2419	10173	0	0.00	0.00	0.00	66
3	7	3547	99.77	3555	2419	10012	0	0.00
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3068	99.77	3075	2419	80909	0	0.00	0.00	0.00	65	65	0.00	0.00	0.00	19.62	17.18
0	0	3068	99.77	3075	2419	10054	0	0.00	0.00	0.00	59	65	0.00	0.00	0.00	19.62	17.18
0	4	3068	99.77	3075	2419	10029	0	0.00
1	1	3068	99.77	3075	2419	10121	0	0.00	0.00	0.00	65
1	5	3068	99.77	3075	2419	10039	0	0.00
2	2	3068	99.77	3075	2419	10069	0	0.00	0.00	0.00	59
2	6	3068	99.77	3075	2419	10276	0	0.00
3	3	3068	99.77	3075	2419	10229	0	0.00	0.00	0.00	64
3	7	3068	99.76	3075	2419	10092	0	0.00
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3083	99.77	3090	2419	80593	0	0.00	0.00	0.00	65	65	0.00	0.00	0.00	19.85	17.43
0	0	3083	99.77	3090	2419	10008	0	0.00	0.00	0.00	58	65	0.00	0.00	0.00	19.85	17.43
0	4	3083	99.77	3090	2419	10020	0	0.00
1	1	3083	99.77	3090	2419	10006	0	0.00	0.00	0.00	64
1	5	3083	99.77	3090	2419	10019	0	0.00
2	2	3083	99.77	3090	2419	10028	0	0.00	0.00	0.00	60
2	6	3083	99.77	3090	2419	10282	0	0.00
3	3	3083	99.77	3090	2419	10221	0	0.00	0.00	0.00	65
3	7	3083	99.77	3090	2419	10009	0	0.00
Core	CPU	Avg_MHz	Busy%	Bzy_MHz	TSC_MHz	IRQ	SMI	CPU%c1	CPU%c6	CPU%c7	CoreTmp	PkgTmp	Pkg%pc2	Pkg%pc6	Pk%pc10	PkgWatt	CorWatt
-	-	3087	99.77	3094	2419	80544	0	0.00	0.00	0.00	65	65	0.00	0.00	0.00	19.92	17.49
0	0	3087	99.77	3094	2419	10009	0	0.00	0.00	0.00	58	65	0.00	0.00	0.00	19.92	17.49
0	4	3087	99.77	3094	2419	10011	0	0.00
1	1	3087	99.77	3094	2419	10012	0	0.00	0.00	0.00	65
1	5	3087	99.77	3094	2419	10009	0	0.00
2	2	3087	99.77	3094	2419	10017	0	0.00	0.00	0.00	59
2	6	3087	99.77	3094	2419	10273	0	0.00
3	3	3087	99.77	3094	2419	10205	0	0.00	0.00	0.00	65
3	7	3087	99.77	3094	2419	10008	0	0.00
```

Idle, 60 s: `perf stat -a -A -e msr/aperf/,msr/mperf/,msr/smi/,msr/tsc/ sleep
60`. `msr/smi/` 0 on all eight CPUs; `msr/tsc/` 145,157,230,321 to
145,157,642,420 per CPU over 60.002902881 s, 2419.2 MHz.

```
 Performance counter stats for 'system wide':

CPU0           98,079,648      msr/aperf/
CPU1          181,130,722      msr/aperf/
CPU2          121,578,728      msr/aperf/
CPU3          140,715,079      msr/aperf/
CPU4           61,297,064      msr/aperf/
CPU5          191,874,303      msr/aperf/
CPU6           96,508,493      msr/aperf/
CPU7           74,201,638      msr/aperf/
CPU0          150,186,840      msr/mperf/
CPU1          192,582,720      msr/mperf/
CPU2          234,799,920      msr/mperf/
CPU3          313,507,656      msr/mperf/
CPU4          156,309,048      msr/mperf/
CPU5          326,606,712      msr/mperf/
CPU6          227,450,760      msr/mperf/
CPU7          156,326,856      msr/mperf/
CPU0                    0      msr/smi/
CPU1                    0      msr/smi/
CPU2                    0      msr/smi/
CPU3                    0      msr/smi/
CPU4                    0      msr/smi/
CPU5                    0      msr/smi/
CPU6                    0      msr/smi/
CPU7                    0      msr/smi/
CPU0      145,157,386,466      msr/tsc/
CPU1      145,157,434,015      msr/tsc/
CPU2      145,157,376,550      msr/tsc/
CPU3      145,157,230,321      msr/tsc/
CPU4      145,157,567,260      msr/tsc/
CPU5      145,157,642,420      msr/tsc/
CPU6      145,157,528,396      msr/tsc/
CPU7      145,157,356,567      msr/tsc/

      60.002902881 seconds time elapsed
```

Owner: the orchestrator, which holds the T14 the exit runs on.
