# Overlay 提交实验 2 原始数据报告

## 环境

| 项目 | 原始值 |
| --- | --- |
| OS | NAME="Rocky Linux"<br>VERSION="10.2 (Red Quartz)"<br>RELEASE_TYPE="stable"<br>ID="rocky"<br>ID_LIKE="rhel centos fedora"<br>VERSION_ID="10.2"<br>PLATFORM_ID="platform:el10"<br>PRETTY_NAME="Rocky Linux 10.2 (Red Quartz)"<br>ANSI_COLOR="0;32"<br>LOGO="fedora-logo-icon"<br>CPE_NAME="cpe:/o:rocky:rocky:10::baseos"<br>HOME_URL="https://rockylinux.org/"<br>VENDOR_NAME="RESF"<br>VENDOR_URL="https://resf.org/"<br>BUG_REPORT_URL="https://bugs.rockylinux.org/"<br>SUPPORT_END="2035-05-31"<br>ROCKY_SUPPORT_PRODUCT="Rocky-Linux-10"<br>ROCKY_SUPPORT_PRODUCT_VERSION="10.2"<br>REDHAT_SUPPORT_PRODUCT="Rocky Linux"<br>REDHAT_SUPPORT_PRODUCT_VERSION="10.2"<br> |
| 内核 | 6.12.0-211.68.1+2.1.el10_2_ciq.x86_64 |
| SELinux | Enforcing |
| 文件系统 | Filesystem     Type 1K-blocks      Used Available Use% Mounted on<br>/dev/nvme0n1p2 xfs  261871664 180015420  81856244  69% /<br> |
| 预热后磁盘（字节） | Filesystem        1B-blocks         Used   Available Use% Mounted on<br>/dev/nvme0n1p2 268156583936 184335790080 83820793856  69% /<br> |
| LATTICE HEAD | 27d349910fff1428eae4f2dab832f10ed377a4ab |
| LANCET 基线 | 67674d308db81a015cf1cff60e2b9d40f0e7ff74 |
| 开始前可用字节（最初 df 工具输出） | 87095607296 |
| 开始 / 场景阈值字节 | [32212254720,16106127360] |
| Python | 3.12.15 (main, Oct  1 2026, 00:00:00) [GCC 14.3.1 20251022 (Red Hat 14.3.1-4)] |
| Git | git version 2.52.0 |
| rustc | rustc 1.96.0 (ac68faa20 2026-05-25) |
| 仓库计数：含 target（条目 / 非目录文件） | {"entries":6832,"files":5900} |
| 仓库计数：不含 target（条目 / 非目录文件） | {"entries":3268,"files":2874} |
| 预热 cargo check --workspace（秒 / code） | [32.91580629599048,0] |
| 计时口径 | 秒；total_s 含 T0、快照、命名空间、脚本、分类/预检/提交、xattr 和内外 Git 观测；不含驱动恢复基线 |
| 提交耗时口径 | commit_s = reflink 前态 + 写回；分类、预检、getfattr 另列 |
| 文件数口径 | lstat，含 .git；目录与文件分别计；符号链接按非目录条目计；不跟随链接 |
| 正式运行数 | 84 |
| 排除的前置失败批次运行数 | 10 |
| 原始环境 | [environment.json](results/environment.json) |

## 表 1：各场景结局（每次运行一行）

| 场景 / 变体 / 重复 | git 策略 | 冲突检查 | 运行 | 结局 | 是否符合期望 | 总耗时 s | 提交耗时 s | 计入正式统计 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| S1/default/1 | P1 | both | [S1-default-P1-1-1794496795061781230](results/S1-default-P1-1-1794496795061781230.json.gz) | committed | True | 4.065472 | 0.004904 | True |
| S1/default/2 | P1 | both | [S1-default-P1-2-1794496799540457144](results/S1-default-P1-2-1794496799540457144.json.gz) | committed | True | 3.954232 | 0.004764 | True |
| S1/default/3 | P1 | both | [S1-default-P1-3-1794496803895224181](results/S1-default-P1-3-1794496803895224181.json.gz) | committed | True | 3.818618 | 0.004598 | True |
| S3/default/1 | P1 | both | [S3-default-P1-1-1794496808124449063](results/S3-default-P1-1-1794496808124449063.json.gz) | conflict | True | 3.862560 | 0 | True |
| S3/default/2 | P1 | both | [S3-default-P1-2-1794496812392685273](results/S3-default-P1-2-1794496812392685273.json.gz) | conflict | True | 3.906459 | 0 | True |
| S3/default/3 | P1 | both | [S3-default-P1-3-1794496816734188342](results/S3-default-P1-3-1794496816734188342.json.gz) | conflict | True | 3.879257 | 0 | True |
| S4/default/1 | P1 | both | [S4-default-P1-1-1794496821044963951](results/S4-default-P1-1-1794496821044963951.json.gz) | committed | True | 3.963216 | 0.004888 | True |
| S4/default/2 | P1 | both | [S4-default-P1-2-1794496825427557238](results/S4-default-P1-2-1794496825427557238.json.gz) | committed | True | 3.900938 | 0.004771 | True |
| S4/default/3 | P1 | both | [S4-default-P1-3-1794496829725077867](results/S4-default-P1-3-1794496829725077867.json.gz) | committed | True | 3.981858 | 0.004761 | True |
| S5/discard/1 | P1 | both | [S5-discard-P1-1-1794496834498925292](results/S5-discard-P1-1-1794496834498925292.json.gz) | failed-discarded | True | 9.115148 | 0 | True |
| S5/keep-regenerable/1 | P1 | both | [S5-keep-regenerable-P1-1-1794496848353379423](results/S5-keep-regenerable-P1-1-1794496848353379423.json.gz) | failed-kept-regenerable | True | 9.077588 | 0.017648 | True |
| S5/discard/2 | P1 | both | [S5-discard-P1-2-1794496858274311842](results/S5-discard-P1-2-1794496858274311842.json.gz) | failed-discarded | True | 9.149725 | 0 | True |
| S5/keep-regenerable/2 | P1 | both | [S5-keep-regenerable-P1-2-1794496872166623811](results/S5-keep-regenerable-P1-2-1794496872166623811.json.gz) | failed-kept-regenerable | True | 9.272179 | 0.017798 | True |
| S5/discard/3 | P1 | both | [S5-discard-P1-3-1794496882430511669](results/S5-discard-P1-3-1794496882430511669.json.gz) | failed-discarded | True | 9.389213 | 0 | True |
| S5/keep-regenerable/3 | P1 | both | [S5-keep-regenerable-P1-3-1794496896904923741](results/S5-keep-regenerable-P1-3-1794496896904923741.json.gz) | failed-kept-regenerable | True | 13.360328 | 0.017737 | True |
| S7/mv/1 | P1 | both | [S7-mv-P1-1-1794496910811559429](results/S7-mv-P1-1-1794496910811559429.json.gz) | committed | True | 6.440857 | 0.005649 | True |
| S7/git-mv/1 | P1 | both | [S7-git-mv-P1-1-1794496917661409991](results/S7-git-mv-P1-1-1794496917661409991.json.gz) | failed-discarded | True | 3.967605 | 0 | True |
| S7/os-rename/1 | P1 | both | [S7-os-rename-P1-1-1794496922024549458](results/S7-os-rename-P1-1-1794496922024549458.json.gz) | failed-discarded | True | 4.070321 | 0 | True |
| S7/mv/2 | P1 | both | [S7-mv-P1-2-1794496926534380352](results/S7-mv-P1-2-1794496926534380352.json.gz) | committed | True | 3.920638 | 0.005197 | True |
| S7/git-mv/2 | P1 | both | [S7-git-mv-P1-2-1794496930865665850](results/S7-git-mv-P1-2-1794496930865665850.json.gz) | failed-discarded | True | 3.989979 | 0 | True |
| S7/os-rename/2 | P1 | both | [S7-os-rename-P1-2-1794496935248808027](results/S7-os-rename-P1-2-1794496935248808027.json.gz) | failed-discarded | True | 3.920207 | 0 | True |
| S7/mv/3 | P1 | both | [S7-mv-P1-3-1794496939563443266](results/S7-mv-P1-3-1794496939563443266.json.gz) | committed | True | 3.873369 | 0.005379 | True |
| S7/git-mv/3 | P1 | both | [S7-git-mv-P1-3-1794496943840987126](results/S7-git-mv-P1-3-1794496943840987126.json.gz) | failed-discarded | True | 3.859398 | 0 | True |
| S7/os-rename/3 | P1 | both | [S7-os-rename-P1-3-1794496948176442545](results/S7-os-rename-P1-3-1794496948176442545.json.gz) | failed-discarded | True | 4.042193 | 0 | True |
| S8/opaque/1 | P1 | both | [S8-opaque-P1-1-1794496952674653368](results/S8-opaque-P1-1-1794496952674653368.json.gz) | committed | True | 3.936860 | 0.005158 | True |
| S8/opaque/2 | P1 | both | [S8-opaque-P1-2-1794496957007254376](results/S8-opaque-P1-2-1794496957007254376.json.gz) | committed | True | 3.972648 | 0.005983 | True |
| S8/opaque/3 | P1 | both | [S8-opaque-P1-3-1794496961372596034](results/S8-opaque-P1-3-1794496961372596034.json.gz) | committed | True | 4.066109 | 0.005202 | True |
| S9/different-B/1 | P1 | both | [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | committed | True | 6.392106 | 0.001844 | True |
| S9/different-A/1 | P1 | both | [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | committed | True | 4.009856 | 0.001705 | True |
| S9/same-B/1 | P1 | both | [S9-same-B-P1-1-1794496972649340842](results/S9-same-B-P1-1-1794496972649340842.json.gz) | conflict | True | 6.348162 | 0 | True |
| S9/same-A/1 | P1 | both | [S9-same-A-P1-1-1794496972648928082](results/S9-same-A-P1-1-1794496972648928082.json.gz) | committed | True | 3.960649 | 0.001714 | True |
| S9/different-A/2 | P1 | both | [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | committed | True | 4.006320 | 0.003802 | True |
| S9/different-B/2 | P1 | both | [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | committed | True | 6.373615 | 0.001856 | True |
| S9/same-A/2 | P1 | both | [S9-same-A-P1-2-1794496986191687581](results/S9-same-A-P1-2-1794496986191687581.json.gz) | committed | True | 3.958451 | 0.001732 | True |
| S9/same-B/2 | P1 | both | [S9-same-B-P1-2-1794496986192093471](results/S9-same-B-P1-2-1794496986192093471.json.gz) | conflict | True | 6.356119 | 0 | True |
| S9/different-A/3 | P1 | both | [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | committed | True | 3.954687 | 0.001852 | True |
| S9/different-B/3 | P1 | both | [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | committed | True | 6.411542 | 0.001701 | True |
| S9/same-A/3 | P1 | both | [S9-same-A-P1-3-1794496999791544818](results/S9-same-A-P1-3-1794496999791544818.json.gz) | committed | True | 3.976107 | 0.001751 | True |
| S9/same-B/3 | P1 | both | [S9-same-B-P1-3-1794496999791966168](results/S9-same-B-P1-3-1794496999791966168.json.gz) | conflict | True | 6.347751 | 0 | True |
| S2/noise/1 | P1 | both | [S2-noise-P1-1-1794497694692569642](results/S2-noise-P1-1-1794497694692569642.json.gz) | committed | True | 41.522950 | 0.005948 | True |
| S2/noise/1 | P2 | both | [S2-noise-P2-1-1794497736792212675](results/S2-noise-P2-1-1794497736792212675.json.gz) | committed | True | 34.765816 | 0.004776 | True |
| S2/noise/1 | P3 | both | [S2-noise-P3-1-1794497771991094191](results/S2-noise-P3-1-1794497771991094191.json.gz) | committed | True | 34.601431 | 0.004910 | True |
| S2/noise/2 | P1 | both | [S2-noise-P1-2-1794497807017717004](results/S2-noise-P1-2-1794497807017717004.json.gz) | committed | True | 34.213448 | 0.005008 | True |
| S2/noise/2 | P2 | both | [S2-noise-P2-2-1794497841655067685](results/S2-noise-P2-2-1794497841655067685.json.gz) | committed | True | 34.426435 | 0.004816 | True |
| S2/noise/2 | P3 | both | [S2-noise-P3-2-1794497876519469892](results/S2-noise-P3-2-1794497876519469892.json.gz) | committed | True | 34.736530 | 0.004820 | True |
| S2/noise/3 | P1 | both | [S2-noise-P1-3-1794497911703667326](results/S2-noise-P1-3-1794497911703667326.json.gz) | committed | True | 34.450654 | 0.004868 | True |
| S2/noise/3 | P2 | both | [S2-noise-P2-3-1794497946579129582](results/S2-noise-P2-3-1794497946579129582.json.gz) | committed | True | 34.592323 | 0.004774 | True |
| S2/noise/3 | P3 | both | [S2-noise-P3-3-1794497981605125568](results/S2-noise-P3-3-1794497981605125568.json.gz) | committed | True | 34.243614 | 0.004728 | True |
| S6/quiet-commit/1 | P1 | both | [S6-quiet-commit-P1-1-1794498018335166402](results/S6-quiet-commit-P1-1-1794498018335166402.json.gz) | failed-discarded | False | 3.483497 | 0 | False |
| S6/quiet-commit/2 | P1 | both | [S6-quiet-commit-P1-2-1794498024355147660](results/S6-quiet-commit-P1-2-1794498024355147660.json.gz) | failed-discarded | False | 3.437243 | 0 | False |
| S6/quiet-commit/3 | P1 | both | [S6-quiet-commit-P1-3-1794498030326384829](results/S6-quiet-commit-P1-3-1794498030326384829.json.gz) | failed-discarded | False | 3.407207 | 0 | False |
| S6/quiet-discard/1 | P1 | both | [S6-quiet-discard-P1-1-1794498036782811223](results/S6-quiet-discard-P1-1-1794498036782811223.json.gz) | failed-discarded | False | 3.570589 | 0 | False |
| S6/quiet-discard/2 | P1 | both | [S6-quiet-discard-P1-2-1794498042840064819](results/S6-quiet-discard-P1-2-1794498042840064819.json.gz) | failed-discarded | False | 3.461551 | 0 | False |
| S6/quiet-discard/3 | P1 | both | [S6-quiet-discard-P1-3-1794498048795038828](results/S6-quiet-discard-P1-3-1794498048795038828.json.gz) | failed-discarded | False | 3.410124 | 0 | False |
| S6/quiet-commit/1 | P2 | both | [S6-quiet-commit-P2-1-1794498054717579339](results/S6-quiet-commit-P2-1-1794498054717579339.json.gz) | failed-discarded | True | 2.926501 | 0 | False |
| S6/quiet-commit/2 | P2 | both | [S6-quiet-commit-P2-2-1794498060116325115](results/S6-quiet-commit-P2-2-1794498060116325115.json.gz) | failed-discarded | True | 3.032937 | 0 | False |
| S6/quiet-commit/3 | P2 | both | [S6-quiet-commit-P2-3-1794498066487962252](results/S6-quiet-commit-P2-3-1794498066487962252.json.gz) | failed-discarded | True | 3.390407 | 0 | False |
| S6/quiet-discard/1 | P2 | both | [S6-quiet-discard-P2-1-1794498072401595663](results/S6-quiet-discard-P2-1-1794498072401595663.json.gz) | interrupted | — | — | — | False |
| S6/quiet-commit/1 | P1 | both | [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) | committed | True | 4.706785 | 0.008359 | True |
| S6/quiet-commit/2 | P1 | both | [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) | committed | True | 4.612186 | 0.008738 | True |
| S6/quiet-commit/3 | P1 | both | [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) | committed | True | 4.722010 | 0.008621 | True |
| S6/quiet-discard/1 | P1 | both | [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) | discarded | True | 4.531936 | 0 | True |
| S6/quiet-discard/2 | P1 | both | [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) | discarded | True | 4.536452 | 0 | True |
| S6/quiet-discard/3 | P1 | both | [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) | discarded | True | 4.525589 | 0 | True |
| S6/quiet-commit/1 | P2 | both | [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) | failed-discarded | True | 2.862403 | 0 | True |
| S6/quiet-commit/2 | P2 | both | [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) | failed-discarded | True | 2.873876 | 0 | True |
| S6/quiet-commit/3 | P2 | both | [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) | failed-discarded | True | 2.911673 | 0 | True |
| S6/quiet-discard/1 | P2 | both | [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) | failed-discarded | True | 2.870396 | 0 | True |
| S6/quiet-discard/2 | P2 | both | [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) | failed-discarded | True | 2.953440 | 0 | True |
| S6/quiet-discard/3 | P2 | both | [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) | failed-discarded | True | 2.945859 | 0 | True |
| S6/quiet-commit/1 | P3 | both | [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) | committed | True | 3.825218 | 0.000004 | True |
| S6/quiet-commit/2 | P3 | both | [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) | committed | True | 3.855693 | 0.000005 | True |
| S6/quiet-commit/3 | P3 | both | [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) | committed | True | 3.894104 | 0.000005 | True |
| S6/quiet-discard/1 | P3 | both | [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) | discarded | False | 3.847592 | 0 | True |
| S6/quiet-discard/2 | P3 | both | [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) | discarded | False | 3.913312 | 0 | True |
| S6/quiet-discard/3 | P3 | both | [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) | discarded | False | 3.821961 | 0 | True |
| S6/noise-commit/1 | P1 | both | [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | conflict | False | 34.710779 | 0 | True |
| S6/noise-commit/2 | P1 | both | [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | conflict | False | 34.996170 | 0 | True |
| S6/noise-commit/3 | P1 | both | [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | conflict | False | 34.954543 | 0 | True |
| S6/noise-discard/1 | P1 | both | [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | discarded | True | 35.176735 | 0 | True |
| S6/noise-discard/2 | P1 | both | [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | discarded | True | 35.183518 | 0 | True |
| S6/noise-discard/3 | P1 | both | [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | discarded | True | 35.144331 | 0 | True |
| S6/noise-commit/1 | P2 | both | [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | failed-discarded | True | 33.118941 | 0 | True |
| S6/noise-commit/2 | P2 | both | [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | failed-discarded | True | 32.996151 | 0 | True |
| S6/noise-commit/3 | P2 | both | [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | failed-discarded | True | 33.573973 | 0 | True |
| S6/noise-discard/1 | P2 | both | [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | failed-discarded | True | 33.466111 | 0 | True |
| S6/noise-discard/2 | P2 | both | [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | failed-discarded | True | 32.984738 | 0 | True |
| S6/noise-discard/3 | P2 | both | [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | failed-discarded | True | 32.955347 | 0 | True |
| S6/noise-commit/1 | P3 | both | [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | committed | True | 34.142455 | 0.000005 | True |
| S6/noise-commit/2 | P3 | both | [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | committed | True | 34.504751 | 0.000005 | True |
| S6/noise-commit/3 | P3 | both | [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | committed | True | 34.563501 | 0.000004 | True |
| S6/noise-discard/1 | P3 | both | [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | discarded | False | 33.982301 | 0 | True |
| S6/noise-discard/2 | P3 | both | [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | discarded | False | 34.597389 | 0 | True |
| S6/noise-discard/3 | P3 | both | [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | discarded | False | 34.349298 | 0 | True |

## 表 2：冲突检查

| 运行 | C1 判出 | C4 判出 | 实际冲突路径 | 外部注入路径 | C1 误报 / 漏报 | C4 误报 / 漏报 |
| --- | --- | --- | --- | --- | --- | --- |
| [S1-default-P1-1-1794496795061781230](results/S1-default-P1-1-1794496795061781230.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S1-default-P1-2-1794496799540457144](results/S1-default-P1-2-1794496799540457144.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S1-default-P1-3-1794496803895224181](results/S1-default-P1-3-1794496803895224181.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S3-default-P1-1-1794496808124449063](results/S3-default-P1-1-1794496808124449063.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | ["README.md"] | [0,0] | [0,0] |
| [S3-default-P1-2-1794496812392685273](results/S3-default-P1-2-1794496812392685273.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | ["README.md"] | [0,0] | [0,0] |
| [S3-default-P1-3-1794496816734188342](results/S3-default-P1-3-1794496816734188342.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | ["README.md"] | [0,0] | [0,0] |
| [S4-default-P1-1-1794496821044963951](results/S4-default-P1-1-1794496821044963951.json.gz) | [] | [] | [] | ["Cargo.toml"] | [0,0] | [0,0] |
| [S4-default-P1-2-1794496825427557238](results/S4-default-P1-2-1794496825427557238.json.gz) | [] | [] | [] | ["Cargo.toml"] | [0,0] | [0,0] |
| [S4-default-P1-3-1794496829725077867](results/S4-default-P1-3-1794496829725077867.json.gz) | [] | [] | [] | ["Cargo.toml"] | [0,0] | [0,0] |
| [S5-discard-P1-1-1794496834498925292](results/S5-discard-P1-1-1794496834498925292.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S5-keep-regenerable-P1-1-1794496848353379423](results/S5-keep-regenerable-P1-1-1794496848353379423.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S5-discard-P1-2-1794496858274311842](results/S5-discard-P1-2-1794496858274311842.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S5-keep-regenerable-P1-2-1794496872166623811](results/S5-keep-regenerable-P1-2-1794496872166623811.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S5-discard-P1-3-1794496882430511669](results/S5-discard-P1-3-1794496882430511669.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S5-keep-regenerable-P1-3-1794496896904923741](results/S5-keep-regenerable-P1-3-1794496896904923741.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-mv-P1-1-1794496910811559429](results/S7-mv-P1-1-1794496910811559429.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-git-mv-P1-1-1794496917661409991](results/S7-git-mv-P1-1-1794496917661409991.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-os-rename-P1-1-1794496922024549458](results/S7-os-rename-P1-1-1794496922024549458.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-mv-P1-2-1794496926534380352](results/S7-mv-P1-2-1794496926534380352.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-git-mv-P1-2-1794496930865665850](results/S7-git-mv-P1-2-1794496930865665850.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-os-rename-P1-2-1794496935248808027](results/S7-os-rename-P1-2-1794496935248808027.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-mv-P1-3-1794496939563443266](results/S7-mv-P1-3-1794496939563443266.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-git-mv-P1-3-1794496943840987126](results/S7-git-mv-P1-3-1794496943840987126.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S7-os-rename-P1-3-1794496948176442545](results/S7-os-rename-P1-3-1794496948176442545.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S8-opaque-P1-1-1794496952674653368](results/S8-opaque-P1-1-1794496952674653368.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S8-opaque-P1-2-1794496957007254376](results/S8-opaque-P1-2-1794496957007254376.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S8-opaque-P1-3-1794496961372596034](results/S8-opaque-P1-3-1794496961372596034.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-same-B-P1-1-1794496972649340842](results/S9-same-B-P1-1-1794496972649340842.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | [] | [0,0] | [0,0] |
| [S9-same-A-P1-1-1794496972648928082](results/S9-same-A-P1-1-1794496972648928082.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-same-A-P1-2-1794496986191687581](results/S9-same-A-P1-2-1794496986191687581.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-same-B-P1-2-1794496986192093471](results/S9-same-B-P1-2-1794496986192093471.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | [] | [0,0] | [0,0] |
| [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-same-A-P1-3-1794496999791544818](results/S9-same-A-P1-3-1794496999791544818.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S9-same-B-P1-3-1794496999791966168](results/S9-same-B-P1-3-1794496999791966168.json.gz) | ["README.md"] | ["README.md"] | ["README.md"] | [] | [0,0] | [0,0] |
| [S2-noise-P1-1-1794497694692569642](results/S2-noise-P1-1-1794497694692569642.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P2-1-1794497736792212675](results/S2-noise-P2-1-1794497736792212675.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P3-1-1794497771991094191](results/S2-noise-P3-1-1794497771991094191.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P1-2-1794497807017717004](results/S2-noise-P1-2-1794497807017717004.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P2-2-1794497841655067685](results/S2-noise-P2-2-1794497841655067685.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P3-2-1794497876519469892](results/S2-noise-P3-2-1794497876519469892.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P1-3-1794497911703667326](results/S2-noise-P1-3-1794497911703667326.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P2-3-1794497946579129582](results/S2-noise-P2-3-1794497946579129582.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S2-noise-P3-3-1794497981605125568](results/S2-noise-P3-3-1794497981605125568.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | [".git/index"] | [".git/index"] | [] | [] | [1,0] | [1,0] |
| [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | [".git/index"] | [".git/index"] | [] | [] | [1,0] | [1,0] |
| [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | [".git/index"] | [".git/index"] | [] | [] | [1,0] | [1,0] |
| [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | [".git/index"] | [".git/index"] | [] | [] | [1,0] | [1,0] |
| [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | [".git/index"] | [".git/index"] | [] | [] | [1,0] | [1,0] |
| [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |
| [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | [] | [] | [] | [] | [0,0] | [0,0] |

## 冲突检查计数口径

| 项 | 值 |
| --- | --- |
| 单位 | 一次运行 × 一条路径；聚合不去除跨运行重复 |
| 真冲突 | S3 的 README.md；S9 same-B 的 README.md |
| E-other | Cargo.toml 不在提交集合，真冲突集合为空 |
| 其他运行 | 背景噪声和 P3 直接 Git 写入不作为人为注入的真实源码冲突 |
| both | 两者各自记录；实际提交决策采用 C1 |
| 丢弃与脚本失败 | 仍分类并计算两种检查结果；按 finish/on-fail 决定是否写回 |
| C4 不存在的路径 | 当前 lstat 为 None 时不判冲突；此为写死 ctime > T0 规则的实现 |

## C1 快照与阶段耗时

| 运行 | 含 target s | 不含 target s | 含 / 不含条目数 | 挂载 s | 脚本 s | 分类 s | 冲突检查 s | reflink s | 写回 s | 属性扫描 s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [S1-default-P1-1-1794496795061781230](results/S1-default-P1-1-1794496795061781230.json.gz) | 0.327882 | 0.150469 | [6833,3269] | 0.004793 | 0.006180 | 0.000382 | 0.000066 | 0.004658 | 0.000246 | 0.000935 |
| [S1-default-P1-2-1794496799540457144](results/S1-default-P1-2-1794496799540457144.json.gz) | 0.347462 | 0.161221 | [6833,3269] | 0.004373 | 0.006054 | 0.000397 | 0.000069 | 0.004552 | 0.000212 | 0.000945 |
| [S1-default-P1-3-1794496803895224181](results/S1-default-P1-3-1794496803895224181.json.gz) | 0.335891 | 0.152639 | [6833,3269] | 0.003704 | 0.005945 | 0.000342 | 0.000064 | 0.004401 | 0.000197 | 0.000911 |
| [S3-default-P1-1-1794496808124449063](results/S3-default-P1-1-1794496808124449063.json.gz) | 0.329242 | 0.151903 | [6833,3269] | 0.003769 | 0.029095 | 0.000345 | 0.000083 | — | — | — |
| [S3-default-P1-2-1794496812392685273](results/S3-default-P1-2-1794496812392685273.json.gz) | 0.329571 | 0.150208 | [6833,3269] | 0.003611 | 0.028443 | 0.000345 | 0.000066 | — | — | — |
| [S3-default-P1-3-1794496816734188342](results/S3-default-P1-3-1794496816734188342.json.gz) | 0.328485 | 0.154558 | [6833,3269] | 0.003606 | 0.028428 | 0.000345 | 0.000064 | — | — | — |
| [S4-default-P1-1-1794496821044963951](results/S4-default-P1-1-1794496821044963951.json.gz) | 0.330038 | 0.160187 | [6833,3269] | 0.003796 | 0.028465 | 0.000374 | 0.000065 | 0.004682 | 0.000206 | 0.000940 |
| [S4-default-P1-2-1794496825427557238](results/S4-default-P1-2-1794496825427557238.json.gz) | 0.330895 | 0.150348 | [6833,3269] | 0.003662 | 0.028207 | 0.000334 | 0.000063 | 0.004570 | 0.000201 | 0.000975 |
| [S4-default-P1-3-1794496829725077867](results/S4-default-P1-3-1794496829725077867.json.gz) | 0.330348 | 0.153318 | [6833,3269] | 0.003795 | 0.028141 | 0.000383 | 0.000070 | 0.004559 | 0.000202 | 0.000922 |
| [S5-discard-P1-1-1794496834498925292](results/S5-discard-P1-1-1794496834498925292.json.gz) | 0.331771 | 0.152762 | [6833,3269] | 0.003677 | 4.503269 | 0.029324 | 0.000100 | — | — | — |
| [S5-keep-regenerable-P1-1-1794496848353379423](results/S5-keep-regenerable-P1-1-1794496848353379423.json.gz) | 0.333567 | 0.152666 | [6833,3269] | 0.003725 | 4.466758 | 0.031855 | 0.000100 | 0.000004 | 0.017645 | 0.021756 |
| [S5-discard-P1-2-1794496858274311842](results/S5-discard-P1-2-1794496858274311842.json.gz) | 0.330866 | 0.152337 | [6833,3269] | 0.003558 | 4.426651 | 0.032993 | 0.000092 | — | — | — |
| [S5-keep-regenerable-P1-2-1794496872166623811](results/S5-keep-regenerable-P1-2-1794496872166623811.json.gz) | 0.333560 | 0.151086 | [6833,3269] | 0.003830 | 4.574317 | 0.028465 | 0.000100 | 0.000004 | 0.017794 | 0.022227 |
| [S5-discard-P1-3-1794496882430511669](results/S5-discard-P1-3-1794496882430511669.json.gz) | 0.338973 | 0.151408 | [6833,3269] | 0.003821 | 4.544968 | 0.028959 | 0.000082 | — | — | — |
| [S5-keep-regenerable-P1-3-1794496896904923741](results/S5-keep-regenerable-P1-3-1794496896904923741.json.gz) | 0.430166 | 0.176252 | [6833,3269] | 0.006511 | 6.142960 | 0.029701 | 0.000113 | 0.000004 | 0.017734 | 0.022218 |
| [S7-mv-P1-1-1794496910811559429](results/S7-mv-P1-1-1794496910811559429.json.gz) | 0.350996 | 0.153389 | [7205,3269] | 0.004052 | 0.023774 | 0.000966 | 0.000961 | 0.005178 | 0.000471 | 0.001129 |
| [S7-git-mv-P1-1-1794496917661409991](results/S7-git-mv-P1-1-1794496917661409991.json.gz) | 0.355129 | 0.151970 | [7205,3269] | 0.003716 | 0.006595 | 0.000218 | 0.000004 | — | — | — |
| [S7-os-rename-P1-1-1794496922024549458](results/S7-os-rename-P1-1-1794496922024549458.json.gz) | 0.433251 | 0.152515 | [7205,3269] | 0.003722 | 0.011731 | 0.000051 | 0.000004 | — | — | — |
| [S7-mv-P1-2-1794496926534380352](results/S7-mv-P1-2-1794496926534380352.json.gz) | 0.357099 | 0.150966 | [7205,3269] | 0.003686 | 0.009153 | 0.000726 | 0.000707 | 0.004734 | 0.000464 | 0.001149 |
| [S7-git-mv-P1-2-1794496930865665850](results/S7-git-mv-P1-2-1794496930865665850.json.gz) | 0.365619 | 0.165064 | [7205,3269] | 0.004147 | 0.006356 | 0.000238 | 0.000004 | — | — | — |
| [S7-os-rename-P1-2-1794496935248808027](results/S7-os-rename-P1-2-1794496935248808027.json.gz) | 0.349393 | 0.151660 | [7205,3269] | 0.003881 | 0.011956 | 0.000073 | 0.000006 | — | — | — |
| [S7-mv-P1-3-1794496939563443266](results/S7-mv-P1-3-1794496939563443266.json.gz) | 0.343596 | 0.149745 | [7205,3269] | 0.003757 | 0.019927 | 0.000719 | 0.000654 | 0.004870 | 0.000509 | 0.001156 |
| [S7-git-mv-P1-3-1794496943840987126](results/S7-git-mv-P1-3-1794496943840987126.json.gz) | 0.343630 | 0.150965 | [7205,3269] | 0.003730 | 0.018137 | 0.000200 | 0.000004 | — | — | — |
| [S7-os-rename-P1-3-1794496948176442545](results/S7-os-rename-P1-3-1794496948176442545.json.gz) | 0.439062 | 0.164485 | [7205,3269] | 0.003874 | 0.012223 | 0.000052 | 0.000004 | — | — | — |
| [S8-opaque-P1-1-1794496952674653368](results/S8-opaque-P1-1-1794496952674653368.json.gz) | 0.349069 | 0.153801 | [7205,3269] | 0.003599 | 0.022628 | 0.000596 | 0.001081 | 0.004835 | 0.000323 | 0.001015 |
| [S8-opaque-P1-2-1794496957007254376](results/S8-opaque-P1-2-1794496957007254376.json.gz) | 0.365801 | 0.161327 | [7205,3269] | 0.003659 | 0.023269 | 0.000633 | 0.001131 | 0.005655 | 0.000328 | 0.001061 |
| [S8-opaque-P1-3-1794496961372596034](results/S8-opaque-P1-3-1794496961372596034.json.gz) | 0.349350 | 0.154599 | [7205,3269] | 0.003663 | 0.018932 | 0.000620 | 0.001088 | 0.004904 | 0.000298 | 0.001022 |
| [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | 0.347942 | 0.151043 | [7205,3269] | 0.003837 | 3.479346 | 0.000178 | 0.000027 | 0.001737 | 0.000107 | 0.000961 |
| [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | 0.347930 | 0.151193 | [7205,3269] | 0.003841 | 0.025495 | 0.000196 | 0.000029 | 0.001606 | 0.000098 | 0.000918 |
| [S9-same-B-P1-1-1794496972649340842](results/S9-same-B-P1-1-1794496972649340842.json.gz) | 0.342965 | 0.150177 | [7205,3269] | 0.003762 | 3.435673 | 0.000309 | 0.000038 | — | — | — |
| [S9-same-A-P1-1-1794496972648928082](results/S9-same-A-P1-1-1794496972648928082.json.gz) | 0.352687 | 0.155856 | [7205,3269] | 0.003483 | 0.025245 | 0.000182 | 0.000027 | 0.001588 | 0.000126 | 0.000845 |
| [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | 0.344894 | 0.149706 | [7205,3269] | 0.003638 | 0.025555 | 0.000180 | 0.000027 | 0.003701 | 0.000101 | 0.000945 |
| [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | 0.344345 | 0.149839 | [7205,3269] | 0.003607 | 3.488280 | 0.000186 | 0.000057 | 0.001748 | 0.000108 | 0.000929 |
| [S9-same-A-P1-2-1794496986191687581](results/S9-same-A-P1-2-1794496986191687581.json.gz) | 0.349196 | 0.173979 | [7205,3269] | 0.003728 | 0.025640 | 0.000174 | 0.000027 | 0.001635 | 0.000096 | 0.000922 |
| [S9-same-B-P1-2-1794496986192093471](results/S9-same-B-P1-2-1794496986192093471.json.gz) | 0.347788 | 0.173332 | [7205,3269] | 0.004112 | 3.415987 | 0.000296 | 0.000037 | — | — | — |
| [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | 0.346245 | 0.151501 | [7205,3269] | 0.003891 | 0.025563 | 0.000185 | 0.000027 | 0.001748 | 0.000105 | 0.000934 |
| [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | 0.346391 | 0.152683 | [7205,3269] | 0.003598 | 3.434560 | 0.000204 | 0.000027 | 0.001596 | 0.000105 | 0.000930 |
| [S9-same-A-P1-3-1794496999791544818](results/S9-same-A-P1-3-1794496999791544818.json.gz) | 0.347836 | 0.152086 | [7205,3269] | 0.003750 | 0.025613 | 0.000199 | 0.000028 | 0.001645 | 0.000106 | 0.000898 |
| [S9-same-B-P1-3-1794496999791966168](results/S9-same-B-P1-3-1794496999791966168.json.gz) | 0.347808 | 0.151516 | [7205,3269] | 0.003733 | 3.455083 | 0.000300 | 0.000041 | — | — | — |
| [S2-noise-P1-1-1794497694692569642](results/S2-noise-P1-1-1794497694692569642.json.gz) | 0.359734 | 0.156007 | [6833,3269] | 0.003887 | 30.041199 | 0.000455 | 0.000083 | 0.005631 | 0.000317 | 0.001686 |
| [S2-noise-P2-1-1794497736792212675](results/S2-noise-P2-1-1794497736792212675.json.gz) | 0.425061 | 0.198624 | [6868,3269] | 0.010929 | 30.035270 | 0.000379 | 0.000081 | 0.004562 | 0.000214 | 0.000927 |
| [S2-noise-P3-1-1794497771991094191](results/S2-noise-P3-1-1794497771991094191.json.gz) | 0.332871 | 0.153619 | [6868,3269] | 0.007098 | 30.013730 | 0.000367 | 0.000066 | 0.004692 | 0.000219 | 0.000893 |
| [S2-noise-P1-2-1794497807017717004](results/S2-noise-P1-2-1794497807017717004.json.gz) | 0.335572 | 0.154667 | [6868,3269] | 0.004686 | 30.029474 | 0.000366 | 0.000066 | 0.004746 | 0.000262 | 0.001000 |
| [S2-noise-P2-2-1794497841655067685](results/S2-noise-P2-2-1794497841655067685.json.gz) | 0.332037 | 0.150516 | [6868,3269] | 0.008959 | 30.033648 | 0.000440 | 0.000069 | 0.004598 | 0.000218 | 0.000921 |
| [S2-noise-P3-2-1794497876519469892](results/S2-noise-P3-2-1794497876519469892.json.gz) | 0.335019 | 0.151746 | [6868,3269] | 0.007195 | 30.027619 | 0.000352 | 0.000089 | 0.004609 | 0.000211 | 0.000933 |
| [S2-noise-P1-3-1794497911703667326](results/S2-noise-P1-3-1794497911703667326.json.gz) | 0.333169 | 0.151647 | [6868,3269] | 0.003842 | 30.026755 | 0.000365 | 0.000065 | 0.004666 | 0.000202 | 0.000938 |
| [S2-noise-P2-3-1794497946579129582](results/S2-noise-P2-3-1794497946579129582.json.gz) | 0.332764 | 0.152669 | [6868,3269] | 0.008967 | 30.017723 | 0.000361 | 0.000067 | 0.004566 | 0.000208 | 0.000947 |
| [S2-noise-P3-3-1794497981605125568](results/S2-noise-P3-3-1794497981605125568.json.gz) | 0.330634 | 0.151458 | [6868,3269] | 0.007257 | 30.038539 | 0.000343 | 0.000065 | 0.004513 | 0.000214 | 0.001090 |
| [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) | 0.327299 | 0.150763 | [6833,3269] | 0.003724 | 1.439517 | 0.050246 | 0.000232 | 0.007775 | 0.000583 | 0.001158 |
| [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) | 0.347202 | 0.158132 | [6833,3269] | 0.003637 | 1.452274 | 0.061270 | 0.000246 | 0.007957 | 0.000780 | 0.001257 |
| [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) | 0.405600 | 0.191750 | [6833,3269] | 0.003755 | 1.424150 | 0.050859 | 0.000241 | 0.007990 | 0.000630 | 0.001184 |
| [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) | 0.330409 | 0.153605 | [6833,3269] | 0.004004 | 1.431775 | 0.076818 | 0.000373 | — | — | — |
| [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) | 0.333228 | 0.154671 | [6833,3269] | 0.003720 | 1.467347 | 0.050106 | 0.000239 | — | — | — |
| [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) | 0.327624 | 0.151682 | [6833,3269] | 0.003607 | 1.554246 | 0.052451 | 0.000241 | — | — | — |
| [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) | 0.325922 | 0.152034 | [6833,3269] | 0.008687 | 0.028112 | 0.000052 | 0.000004 | — | — | — |
| [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) | 0.328994 | 0.149405 | [6833,3269] | 0.008932 | 0.028180 | 0.000051 | 0.000004 | — | — | — |
| [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) | 0.333915 | 0.154504 | [6833,3269] | 0.008833 | 0.028567 | 0.000051 | 0.000004 | — | — | — |
| [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) | 0.328412 | 0.151327 | [6833,3269] | 0.009056 | 0.028133 | 0.000051 | 0.000004 | — | — | — |
| [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) | 0.329039 | 0.152620 | [6833,3269] | 0.008960 | 0.028498 | 0.000054 | 0.000004 | — | — | — |
| [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) | 0.346976 | 0.159133 | [6833,3269] | 0.009004 | 0.028459 | 0.000053 | 0.000004 | — | — | — |
| [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) | 0.329538 | 0.152219 | [6833,3269] | 0.007134 | 0.957816 | 0.000056 | 0.000004 | 0.000003 | 0.000001 | 0.000006 |
| [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) | 0.328124 | 0.150548 | [6833,3269] | 0.007134 | 1.000441 | 0.000054 | 0.000004 | 0.000003 | 0.000001 | 0.000007 |
| [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) | 0.326901 | 0.150095 | [6833,3269] | 0.006994 | 1.053847 | 0.000055 | 0.000005 | 0.000003 | 0.000001 | 0.000006 |
| [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) | 0.335627 | 0.157150 | [6833,3269] | 0.007496 | 0.993672 | 0.000051 | 0.000004 | — | — | — |
| [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) | 0.325454 | 0.149868 | [6833,3269] | 0.007007 | 1.031532 | 0.000052 | 0.000004 | — | — | — |
| [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) | 0.324319 | 0.149853 | [6833,3269] | 0.006980 | 0.973190 | 0.000051 | 0.000004 | — | — | — |
| [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | 0.326342 | 0.149926 | [6833,3269] | 0.003759 | 31.482313 | 0.050174 | 0.000240 | — | — | — |
| [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | 0.329828 | 0.150419 | [6868,3269] | 0.003714 | 31.414584 | 0.050730 | 0.000241 | — | — | — |
| [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | 0.328710 | 0.151891 | [6868,3269] | 0.003848 | 31.431106 | 0.050103 | 0.000235 | — | — | — |
| [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | 0.328595 | 0.151489 | [6868,3269] | 0.003707 | 31.466865 | 0.050008 | 0.000238 | — | — | — |
| [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | 0.332250 | 0.153087 | [6868,3269] | 0.003743 | 31.459476 | 0.049940 | 0.000230 | — | — | — |
| [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | 0.332982 | 0.154713 | [6868,3269] | 0.003705 | 31.436705 | 0.049966 | 0.000253 | — | — | — |
| [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | 0.331759 | 0.153297 | [6868,3269] | 0.009030 | 30.028857 | 0.000054 | 0.000004 | — | — | — |
| [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | 0.331173 | 0.150751 | [6868,3269] | 0.008676 | 30.030580 | 0.000055 | 0.000004 | — | — | — |
| [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | 0.331131 | 0.150682 | [6868,3269] | 0.009100 | 30.037402 | 0.000054 | 0.000004 | — | — | — |
| [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | 0.332059 | 0.154348 | [6868,3269] | 0.008971 | 30.038511 | 0.000057 | 0.000005 | — | — | — |
| [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | 0.330929 | 0.150311 | [6868,3269] | 0.008903 | 30.023626 | 0.000053 | 0.000004 | — | — | — |
| [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | 0.326981 | 0.150496 | [6868,3269] | 0.009140 | 30.044460 | 0.000053 | 0.000004 | — | — | — |
| [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | 0.330864 | 0.152527 | [6868,3269] | 0.007078 | 31.030240 | 0.000053 | 0.000004 | 0.000003 | 0.000002 | 0.000006 |
| [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | 0.328015 | 0.149851 | [6868,3269] | 0.007034 | 31.119568 | 0.000052 | 0.000004 | 0.000003 | 0.000002 | 0.000006 |
| [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | 0.330083 | 0.151775 | [6868,3269] | 0.007422 | 30.998683 | 0.000052 | 0.000004 | 0.000003 | 0.000001 | 0.000005 |
| [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | 0.330420 | 0.151790 | [6868,3269] | 0.007189 | 30.995728 | 0.000053 | 0.000005 | — | — | — |
| [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | 0.326654 | 0.149782 | [6868,3269] | 0.007028 | 31.035830 | 0.000053 | 0.000004 | — | — | — |
| [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | 0.330883 | 0.150538 | [6868,3269] | 0.007046 | 31.172507 | 0.000054 | 0.000004 | — | — | — |

## 表 3：上层构成

| 运行 | .git 条目 | target 条目 / 常规文件字节 | 其他条目 | 只改时间戳条目 | opaque / redirect | 提交后 overlay 属性残留 |
| --- | --- | --- | --- | --- | --- | --- |
| [S1-default-P1-1-1794496795061781230](results/S1-default-P1-1-1794496795061781230.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S1-default-P1-2-1794496799540457144](results/S1-default-P1-2-1794496799540457144.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S1-default-P1-3-1794496803895224181](results/S1-default-P1-3-1794496803895224181.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S3-default-P1-1-1794496808124449063](results/S3-default-P1-1-1794496808124449063.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 未提交 / 未扫描 |
| [S3-default-P1-2-1794496812392685273](results/S3-default-P1-2-1794496812392685273.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 未提交 / 未扫描 |
| [S3-default-P1-3-1794496816734188342](results/S3-default-P1-3-1794496816734188342.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 未提交 / 未扫描 |
| [S4-default-P1-1-1794496821044963951](results/S4-default-P1-1-1794496821044963951.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S4-default-P1-2-1794496825427557238](results/S4-default-P1-2-1794496825427557238.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S4-default-P1-3-1794496829725077867](results/S4-default-P1-3-1794496829725077867.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S5-discard-P1-1-1794496834498925292](results/S5-discard-P1-1-1794496834498925292.json.gz) | 0 | [379,102748551] | 4 | 2 | [0,0] | 未提交 / 未扫描 |
| [S5-keep-regenerable-P1-1-1794496848353379423](results/S5-keep-regenerable-P1-1-1794496848353379423.json.gz) | 0 | [379,102748550] | 4 | 2 | [0,0] | 0 |
| [S5-discard-P1-2-1794496858274311842](results/S5-discard-P1-2-1794496858274311842.json.gz) | 0 | [379,102748553] | 4 | 2 | [0,0] | 未提交 / 未扫描 |
| [S5-keep-regenerable-P1-2-1794496872166623811](results/S5-keep-regenerable-P1-2-1794496872166623811.json.gz) | 0 | [379,102748552] | 4 | 2 | [0,0] | 0 |
| [S5-discard-P1-3-1794496882430511669](results/S5-discard-P1-3-1794496882430511669.json.gz) | 0 | [379,102748553] | 4 | 2 | [0,0] | 未提交 / 未扫描 |
| [S5-keep-regenerable-P1-3-1794496896904923741](results/S5-keep-regenerable-P1-3-1794496896904923741.json.gz) | 0 | [379,102748551] | 4 | 2 | [0,0] | 0 |
| [S7-mv-P1-1-1794496910811559429](results/S7-mv-P1-1-1794496910811559429.json.gz) | 0 | [0,0] | 7 | 0 | [0,0] | 0 |
| [S7-git-mv-P1-1-1794496917661409991](results/S7-git-mv-P1-1-1794496917661409991.json.gz) | 1 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S7-os-rename-P1-1-1794496922024549458](results/S7-os-rename-P1-1-1794496922024549458.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S7-mv-P1-2-1794496926534380352](results/S7-mv-P1-2-1794496926534380352.json.gz) | 0 | [0,0] | 7 | 0 | [0,0] | 0 |
| [S7-git-mv-P1-2-1794496930865665850](results/S7-git-mv-P1-2-1794496930865665850.json.gz) | 1 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S7-os-rename-P1-2-1794496935248808027](results/S7-os-rename-P1-2-1794496935248808027.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S7-mv-P1-3-1794496939563443266](results/S7-mv-P1-3-1794496939563443266.json.gz) | 0 | [0,0] | 7 | 0 | [0,0] | 0 |
| [S7-git-mv-P1-3-1794496943840987126](results/S7-git-mv-P1-3-1794496943840987126.json.gz) | 1 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S7-os-rename-P1-3-1794496948176442545](results/S7-os-rename-P1-3-1794496948176442545.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S8-opaque-P1-1-1794496952674653368](results/S8-opaque-P1-1-1794496952674653368.json.gz) | 0 | [0,0] | 3 | 0 | [1,0] | 0 |
| [S8-opaque-P1-2-1794496957007254376](results/S8-opaque-P1-2-1794496957007254376.json.gz) | 0 | [0,0] | 3 | 0 | [1,0] | 0 |
| [S8-opaque-P1-3-1794496961372596034](results/S8-opaque-P1-3-1794496961372596034.json.gz) | 0 | [0,0] | 3 | 0 | [1,0] | 0 |
| [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-same-B-P1-1-1794496972649340842](results/S9-same-B-P1-1-1794496972649340842.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 未提交 / 未扫描 |
| [S9-same-A-P1-1-1794496972648928082](results/S9-same-A-P1-1-1794496972648928082.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-same-A-P1-2-1794496986191687581](results/S9-same-A-P1-2-1794496986191687581.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-same-B-P1-2-1794496986192093471](results/S9-same-B-P1-2-1794496986192093471.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 未提交 / 未扫描 |
| [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-same-A-P1-3-1794496999791544818](results/S9-same-A-P1-3-1794496999791544818.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 0 |
| [S9-same-B-P1-3-1794496999791966168](results/S9-same-B-P1-3-1794496999791966168.json.gz) | 0 | [0,0] | 1 | 0 | [0,0] | 未提交 / 未扫描 |
| [S2-noise-P1-1-1794497694692569642](results/S2-noise-P1-1-1794497694692569642.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P2-1-1794497736792212675](results/S2-noise-P2-1-1794497736792212675.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P3-1-1794497771991094191](results/S2-noise-P3-1-1794497771991094191.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P1-2-1794497807017717004](results/S2-noise-P1-2-1794497807017717004.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P2-2-1794497841655067685](results/S2-noise-P2-2-1794497841655067685.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P3-2-1794497876519469892](results/S2-noise-P3-2-1794497876519469892.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P1-3-1794497911703667326](results/S2-noise-P1-3-1794497911703667326.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P2-3-1794497946579129582](results/S2-noise-P2-3-1794497946579129582.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S2-noise-P3-3-1794497981605125568](results/S2-noise-P3-3-1794497981605125568.json.gz) | 0 | [0,0] | 3 | 0 | [0,0] | 0 |
| [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 0 |
| [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) | 360 | [0,0] | 0 | 200 | [0,0] | 0 |
| [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) | 360 | [0,0] | 0 | 200 | [0,0] | 0 |
| [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) | 360 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) | 360 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | 360 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | 361 | [0,0] | 0 | 200 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |
| [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | 0 | [0,0] | 0 | 0 | [0,0] | 未提交 / 未扫描 |

## 表 4：.git 策略（S6）

| 策略 | 噪声 | 重复 | 提交脚本 code | 丢弃脚本 code | 提交后 status / fsck | 丢弃后 status / fsck | 噪声 index 冲突 C1 / C4 | 运行 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | quiet | 1 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) / [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) |
| P1 | quiet | 2 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) / [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) |
| P1 | quiet | 3 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) / [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) |
| P1 | noise | 1 | 0 | 0 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [2,2] | [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) / [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) |
| P1 | noise | 2 | 0 | 0 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [1,1] | [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) / [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) |
| P1 | noise | 3 | 0 | 0 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [2,2] | [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) / [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) |
| P2 | quiet | 1 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) / [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) |
| P2 | quiet | 2 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) / [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) |
| P2 | quiet | 3 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) / [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) |
| P2 | noise | 1 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) / [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) |
| P2 | noise | 2 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) / [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) |
| P2 | noise | 3 | 128 | 128 | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | {"status_code":0,"status":" M README.md\n","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"main"} | [0,0] | [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) / [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) |
| P3 | quiet | 1 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) / [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) |
| P3 | quiet | 2 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) / [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) |
| P3 | quiet | 3 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) / [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) |
| P3 | noise | 1 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) / [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) |
| P3 | noise | 2 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) / [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) |
| P3 | noise | 3 | 0 | 0 | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | {"status_code":0,"status":"","fsck_code":0,"fsck_stdout_lines":467,"fsck_stderr":"","branch":"t2"} | [0,0] | [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) / [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) |

## S6 Git 原始状态校验

| 运行 | HEAD 变化 | after status = before | after fsck stdout = before | fsck code | 全部判定字段 |
| --- | --- | --- | --- | --- | --- |
| [S6-quiet-commit-P1-1-1794498139410690948](results/S6-quiet-commit-P1-1-1794498139410690948.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-commit-P1-2-1794498146932130479](results/S6-quiet-commit-P1-2-1794498146932130479.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-commit-P1-3-1794498154279901516](results/S6-quiet-commit-P1-3-1794498154279901516.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-discard-P1-1-1794498161583492925](results/S6-quiet-discard-P1-1-1794498161583492925.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-discard-P1-2-1794498168700516039](results/S6-quiet-discard-P1-2-1794498168700516039.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-discard-P1-3-1794498175804025684](results/S6-quiet-discard-P1-3-1794498175804025684.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-commit-P2-1-1794498182789891061](results/S6-quiet-commit-P2-1-1794498182789891061.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-commit-P2-2-1794498188200965167](results/S6-quiet-commit-P2-2-1794498188200965167.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-commit-P2-3-1794498193576610594](results/S6-quiet-commit-P2-3-1794498193576610594.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-discard-P2-1-1794498199001173870](results/S6-quiet-discard-P2-1-1794498199001173870.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-discard-P2-2-1794498204346115878](results/S6-quiet-discard-P2-2-1794498204346115878.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-discard-P2-3-1794498209766102013](results/S6-quiet-discard-P2-3-1794498209766102013.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-quiet-commit-P3-1-1794498215192284499](results/S6-quiet-commit-P3-1-1794498215192284499.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-commit-P3-2-1794498221504701487](results/S6-quiet-commit-P3-2-1794498221504701487.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-commit-P3-3-1794498227838543755](results/S6-quiet-commit-P3-3-1794498227838543755.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-quiet-discard-P3-1-1794498234185924533](results/S6-quiet-discard-P3-1-1794498234185924533.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |
| [S6-quiet-discard-P3-2-1794498240504447201](results/S6-quiet-discard-P3-2-1794498240504447201.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |
| [S6-quiet-discard-P3-3-1794498246865077248](results/S6-quiet-discard-P3-3-1794498246865077248.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |
| [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":false,"status_clean":false,"branch_t2":false} |
| [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":false,"status_clean":false,"branch_t2":false} |
| [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":false,"status_clean":false,"branch_t2":false} |
| [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | False | True | True | 0 | {"script":true,"fsck":true,"head":true,"status_restored":true,"branch_restored":true} |
| [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":true,"status_clean":true,"branch_t2":true} |
| [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |
| [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |
| [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | True | False | True | 0 | {"script":true,"fsck":true,"head":false,"status_restored":false,"branch_restored":false} |

## 表 5：S5 重新构建耗时

| 运行 | 失败策略 | 脚本 code | target 条目 / 字节 | 只改时间戳条目 | 下一次 cargo build -p net-guard s | code |
| --- | --- | --- | --- | --- | --- | --- |
| [S5-discard-P1-1-1794496834498925292](results/S5-discard-P1-1-1794496834498925292.json.gz) | discard | 101 | [379,102748551] | 2 | 4.002468 | 0 |
| [S5-keep-regenerable-P1-1-1794496848353379423](results/S5-keep-regenerable-P1-1-1794496848353379423.json.gz) | keep-regenerable | 101 | [379,102748550] | 2 | 0.105101 | 0 |
| [S5-discard-P1-2-1794496858274311842](results/S5-discard-P1-2-1794496858274311842.json.gz) | discard | 101 | [379,102748553] | 2 | 3.988324 | 0 |
| [S5-keep-regenerable-P1-2-1794496872166623811](results/S5-keep-regenerable-P1-2-1794496872166623811.json.gz) | keep-regenerable | 101 | [379,102748552] | 2 | 0.152324 | 0 |
| [S5-discard-P1-3-1794496882430511669](results/S5-discard-P1-3-1794496882430511669.json.gz) | discard | 101 | [379,102748553] | 2 | 4.095832 | 0 |
| [S5-keep-regenerable-P1-3-1794496896904923741](results/S5-keep-regenerable-P1-3-1794496896904923741.json.gz) | keep-regenerable | 101 | [379,102748551] | 2 | 0.106880 | 0 |

## S5 crate 产物大小

| crate | 自有产物逻辑字节 | 文件数 | 选用 |
| --- | --- | --- | --- |
| net-guard | 1520454 | 11 | True |
| lattice-store | 2676569 | 21 | False |
| lattice-profile | 3717090 | 11 | False |
| lattice-agent | 4942515 | 11 | False |
| xtask | 6248030 | 10 | False |
| lattice-cli | 10533126 | 10 | False |
| lattice-conversation | 12600771 | 11 | False |
| lattice-tui | 16129385 | 11 | False |
| lattice-system-harness | 18707246 | 21 | False |
| lattice-session | 22030290 | 11 | False |
| lattice-llm | 40397125 | 11 | False |
| lattice-api | 43599865 | 11 | False |

## 表 6：S7 各命令的结果

| 运行 | 命令 | code | EXDEV | stderr 原文 |
| --- | --- | --- | --- | --- |
| [S7-mv-P1-1-1794496910811559429](results/S7-mv-P1-1-1794496910811559429.json.gz) | mv crates/net-guard crates/net-guard2 | 0 | False |  |
| [S7-git-mv-P1-1-1794496917661409991](results/S7-git-mv-P1-1-1794496917661409991.json.gz) | git mv crates/net-guard crates/net-guard2 | 128 | True | fatal: renaming 'crates/net-guard' failed: Invalid cross-device link<br> |
| [S7-os-rename-P1-1-1794496922024549458](results/S7-os-rename-P1-1-1794496922024549458.json.gz) | python3 -c "import os; os.rename('crates/net-guard','crates/net-guard2')" | 1 | True | Traceback (most recent call last):<br>  File "<string>", line 1, in <module><br>OSError: [Errno 18] Invalid cross-device link: 'crates/net-guard' -> 'crates/net-guard2'<br> |
| [S7-mv-P1-2-1794496926534380352](results/S7-mv-P1-2-1794496926534380352.json.gz) | mv crates/net-guard crates/net-guard2 | 0 | False |  |
| [S7-git-mv-P1-2-1794496930865665850](results/S7-git-mv-P1-2-1794496930865665850.json.gz) | git mv crates/net-guard crates/net-guard2 | 128 | True | fatal: renaming 'crates/net-guard' failed: Invalid cross-device link<br> |
| [S7-os-rename-P1-2-1794496935248808027](results/S7-os-rename-P1-2-1794496935248808027.json.gz) | python3 -c "import os; os.rename('crates/net-guard','crates/net-guard2')" | 1 | True | Traceback (most recent call last):<br>  File "<string>", line 1, in <module><br>OSError: [Errno 18] Invalid cross-device link: 'crates/net-guard' -> 'crates/net-guard2'<br> |
| [S7-mv-P1-3-1794496939563443266](results/S7-mv-P1-3-1794496939563443266.json.gz) | mv crates/net-guard crates/net-guard2 | 0 | False |  |
| [S7-git-mv-P1-3-1794496943840987126](results/S7-git-mv-P1-3-1794496943840987126.json.gz) | git mv crates/net-guard crates/net-guard2 | 128 | True | fatal: renaming 'crates/net-guard' failed: Invalid cross-device link<br> |
| [S7-os-rename-P1-3-1794496948176442545](results/S7-os-rename-P1-3-1794496948176442545.json.gz) | python3 -c "import os; os.rename('crates/net-guard','crates/net-guard2')" | 1 | True | Traceback (most recent call last):<br>  File "<string>", line 1, in <module><br>OSError: [Errno 18] Invalid cross-device link: 'crates/net-guard' -> 'crates/net-guard2'<br> |

## 表 7：S9 最终内容

| 运行 | 提交次序 | 结局 | 路径 | 最终首行 | 最终内容 SHA-256 | 丢失/拒绝的修改 | 完整内容 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-B-P1-1-1794496965846057128](results/S9-different-B-P1-1-1794496965846057128.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-A-P1-1-1794496965845607328](results/S9-different-A-P1-1-1794496965845607328.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-same-B-P1-1-1794496972649340842](results/S9-same-B-P1-1-1794496972649340842.json.gz) | ["A","B"] | conflict | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |
| [S9-same-A-P1-1-1794496972648928082](results/S9-same-A-P1-1-1794496972648928082.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |
| [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-A-P1-2-1794496979408051277](results/S9-different-A-P1-2-1794496979408051277.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-B-P1-2-1794496979408459117](results/S9-different-B-P1-2-1794496979408459117.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-same-A-P1-2-1794496986191687581](results/S9-same-A-P1-2-1794496986191687581.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |
| [S9-same-B-P1-2-1794496986192093471](results/S9-same-B-P1-2-1794496986192093471.json.gz) | ["A","B"] | conflict | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |
| [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-A-P1-3-1794496992962339005](results/S9-different-A-P1-3-1794496992962339005.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | [] | JSON observed.README.md |
| [S9-different-B-P1-3-1794496992962740045](results/S9-different-B-P1-3-1794496992962740045.json.gz) | ["A","B"] | committed | LICENSE | B  | f61dd7c7c367b13f65b4e5b3b5b57fd67393c3b97d24679906fe60485d425764 | [] | JSON observed.LICENSE |
| [S9-same-A-P1-3-1794496999791544818](results/S9-same-A-P1-3-1794496999791544818.json.gz) | ["A","B"] | committed | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |
| [S9-same-B-P1-3-1794496999791966168](results/S9-same-B-P1-3-1794496999791966168.json.gz) | ["A","B"] | conflict | README.md | A # LATTICE | 565bdeae4594c1571585ae355e76ce03c5069785e9d2a8a3288ba066702d3161 | ["B（冲突拒绝）"] | JSON observed.README.md |

## 真实写入统计：calibration-1794497662850941537

| 记录段 UTC | 覆盖秒 / 小时 | 全部事件 | 主 .git/index | 其他 .git | 源码/其他 | 其他 Agent 沙箱 | target 汇总行 | overflow |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-11-12T07:43:07.540799+00:00 → 2026-11-12T08:10:28.168000+00:00 | [1640.6272008419037,0.4557297780116399] | 385 | 0 | 288 | 0 | 96 | 0 | 0 |
| 2026-11-12T15:01:15.201552+00:00 → 2026-11-12T15:34:22.850934+00:00 | [1987.6493816375732,0.5521248282326592] | 887 | 0 | 796 | 0 | 90 | 0 | 0 |

## 真实写入统计口径

| 项 | 原始值 |
| --- | --- |
| 两段覆盖秒数 | 3628.276582 |
| 覆盖不足 24 小时 | True |
| 第一段截止 | 最后一条事件；已知进程于约 08:10 UTC 意外退出 |
| 第二段截止 | 校准冻结时间；服务当时 active (running) |
| 其他 Agent 沙箱来源 | 维护者核实：同一仓库另一个 Codex 会话的 bwrap 临时只读 tmpfs 挂载点 |
| target_per_minute.jsonl | 本次冻结文件 0 字节 / 0 行；无可报告的 target 写入计数 |
| overflow | 按 kind=overflow 计；原始文件没有该类记录 |
| 事件字段 | create/close_write/delete/attrib/moved_from/moved_to；同一次操作可生成多条事件；空 ev 原样保留 |
| start 事件 | 每段 1 条 start；计入全部事件，不计入路径类别 |
| 校准源 | {"events.jsonl":{"path":"/home/gcp-vegas/exp/overlay/lattice-writes/events.jsonl","bytes":130378,"sha256":"19aaa3a01d8bb1c7763ecefae08ce15214a88450d73889d7a14b6bc1133ecc38","truncated_tail_bytes":0},"target_per_minute.jsonl":{"path":"/home/gcp-vegas/exp/overlay/lattice-writes/target_per_minute.jsonl","bytes":0,"sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","truncated_tail_bytes":0}} |

## 真实写入路径频数：段 1

| 路径 | 事件条数 | 事件 / 分钟 |
| --- | --- | --- |
| .aws | 48 | 1.755426 |
| .codex | 48 | 1.755426 |
| .git/FETCH_HEAD | 50 | 1.828569 |
| .git/config | 3 | 0.109714 |
| .git/config.lock | 15 | 0.548571 |
| .git/index.lock | 3 | 0.109714 |
| .git/logs/refs/heads/test | 1 | 0.036571 |
| .git/logs/refs/heads/test/332-wire-shape-baseline | 1 | 0.036571 |
| .git/logs/refs/remotes/origin/test | 1 | 0.036571 |
| .git/logs/refs/remotes/origin/test/332-wire-shape-baseline | 1 | 0.036571 |
| .git/objects/0c/2a12277181fa1f828a3df2ed239be6ce490a75 | 1 | 0.036571 |
| .git/objects/0d/ec5cd6218040a1d624ec2f4b9da629d88d9dfe | 1 | 0.036571 |
| .git/objects/0d/tmp_obj_cHdjE0 | 3 | 0.109714 |
| .git/objects/10/5af080b994965227be1a2df0e77aa56deb741b | 1 | 0.036571 |
| .git/objects/10/tmp_obj_5FGAUP | 3 | 0.109714 |
| .git/objects/28/65b7a073970706bd604cf9f05015bbc5c1a80d | 1 | 0.036571 |
| .git/objects/4b/dd6b0bf3c320a5c268ec464d267c5bb6fff653 | 1 | 0.036571 |
| .git/objects/4b/tmp_obj_hu0On2 | 3 | 0.109714 |
| .git/objects/5d/eddc5241c4546b1b01cf19c615619234c07639 | 1 | 0.036571 |
| .git/objects/5d/tmp_obj_hPfFaQ | 3 | 0.109714 |
| .git/objects/70/94803bc399bd23b1ec4cd01bfea5a8cd253661 | 1 | 0.036571 |
| .git/objects/72/329a7e3a01714e697add843097c61af26f5df6 | 1 | 0.036571 |
| .git/objects/73/6828ab8a3d3385999f3106731b479ebd41f6bf | 1 | 0.036571 |
| .git/objects/73/tmp_obj_co0Gxm | 3 | 0.109714 |
| .git/objects/91/bccce6b5b0942648677496b5dce70ed0d06823 | 1 | 0.036571 |
| .git/objects/91/tmp_obj_gaSUvF | 3 | 0.109714 |
| .git/objects/d8/9cde0f28f6074330425c314cf2a8905a0b78b7 | 1 | 0.036571 |
| .git/objects/d8/tmp_obj_bBtaAu | 3 | 0.109714 |
| .git/objects/e2/2aea6a11382e047f3abeb4eeba99801e1108bf | 1 | 0.036571 |
| .git/objects/e2/tmp_obj_pRyRKK | 3 | 0.109714 |
| .git/objects/f4/c0c912d2fce9dcdbaeaf5dc61484673804ab18 | 1 | 0.036571 |
| .git/objects/f7/54e0954a4642af6eac4122e3866e4a74091da8 | 1 | 0.036571 |
| .git/objects/f7/tmp_obj_WkzSI0 | 3 | 0.109714 |
| .git/objects/maintenance.lock | 3 | 0.109714 |
| .git/packed-refs.lock | 9 | 0.329142 |
| .git/refs/heads/test | 1 | 0.036571 |
| .git/refs/heads/test/332-wire-shape-baseline | 2 | 0.073143 |
| .git/refs/heads/test/332-wire-shape-baseline.lock | 4 | 0.146286 |
| .git/refs/remotes/origin/HEAD.lock | 75 | 2.742853 |
| .git/refs/remotes/origin/test | 1 | 0.036571 |
| .git/refs/remotes/origin/test/332-wire-shape-baseline | 1 | 0.036571 |
| .git/refs/remotes/origin/test/332-wire-shape-baseline.lock | 3 | 0.109714 |
| .git/refs/t3/orchestration-v2/checkpoints/MDIzZGU4YzUzY2RmN2U4ZGMzMzAwODMyMjA2ODI5OWI/ordinal/37 | 1 | 0.036571 |
| .git/refs/t3/orchestration-v2/checkpoints/MDIzZGU4YzUzY2RmN2U4ZGMzMzAwODMyMjA2ODI5OWI/ordinal/37.lock | 3 | 0.109714 |
| .git/refs/t3/orchestration-v2/checkpoints/OGFiZmQwZTg1MzAwNWQ3MTA2OGNlYzUzMWUyZjE1NzE | 1 | 0.036571 |
| .git/refs/t3/orchestration-v2/checkpoints/OGFiZmQwZTg1MzAwNWQ3MTA2OGNlYzUzMWUyZjE1NzE/ordinal/0 | 1 | 0.036571 |
| .git/refs/t3/orchestration-v2/checkpoints/OGFiZmQwZTg1MzAwNWQ3MTA2OGNlYzUzMWUyZjE1NzE/ordinal/0.lock | 2 | 0.073143 |
| .git/t3-checkpoint-index-74e70f7f-aa9a-4057-8772-eca90d2dc064 | 7 | 0.256000 |
| .git/t3-checkpoint-index-74e70f7f-aa9a-4057-8772-eca90d2dc064.lock | 9 | 0.329142 |
| .git/t3-checkpoint-index-e1aedec2-932e-425b-8dfa-bf4584c761b6 | 8 | 0.292571 |
| .git/t3-checkpoint-index-e1aedec2-932e-425b-8dfa-bf4584c761b6.lock | 9 | 0.329142 |
| .git/worktrees/t3-12f724c7/AUTO_MERGE.lock | 9 | 0.329142 |
| .git/worktrees/t3-12f724c7/COMMIT_EDITMSG | 2 | 0.073143 |
| .git/worktrees/t3-12f724c7/HEAD | 1 | 0.036571 |
| .git/worktrees/t3-12f724c7/HEAD.lock | 6 | 0.219428 |
| .git/worktrees/t3-12f724c7/index | 3 | 0.109714 |
| .git/worktrees/t3-12f724c7/index.lock | 9 | 0.329142 |
| .git/worktrees/t3-12f724c7/logs/HEAD | 2 | 0.073143 |

## 真实写入路径频数：段 2

| 路径 | 事件条数 | 事件 / 分钟 |
| --- | --- | --- |
| .aws | 45 | 1.358388 |
| .codex | 45 | 1.358388 |
| .git/FETCH_HEAD | 28 | 0.845219 |
| .git/objects/00/532b195e7480fa9cb54fdb7088006a0a03545c | 1 | 0.030186 |
| .git/objects/00/tmp_obj_RjZaaq | 3 | 0.090559 |
| .git/objects/04/bad3278522f8521ddd387aadf07641560cc399 | 1 | 0.030186 |
| .git/objects/04/tmp_obj_QKkcre | 3 | 0.090559 |
| .git/objects/05/81cf755631ea33c9c41859907f236feff139ac | 5 | 0.150932 |
| .git/objects/0c/1ab8cde657627011592c44265174260412a895 | 1 | 0.030186 |
| .git/objects/0c/tmp_obj_eGRMU7 | 3 | 0.090559 |
| .git/objects/0d/6ede91d1fef79bd38354c70a2dc34da0550f59 | 5 | 0.150932 |
| .git/objects/0f/e23d5749bcf927ec39691735c20f1ab607fb35 | 1 | 0.030186 |
| .git/objects/0f/tmp_obj_vbEIir | 3 | 0.090559 |
| .git/objects/11/6f6d0690f3a070e90a88c0e5cc652120c2509a | 1 | 0.030186 |
| .git/objects/11/tmp_obj_AZwsBL | 3 | 0.090559 |
| .git/objects/14/5314fc264d2ba5462ab135092502fa9c03da1f | 1 | 0.030186 |
| .git/objects/14/79b112c4b7949258c7ab676b75c1c708d95b80 | 1 | 0.030186 |
| .git/objects/14/tmp_obj_H80wei | 3 | 0.090559 |
| .git/objects/14/tmp_obj_UQ4kRF | 3 | 0.090559 |
| .git/objects/16/2d6fb492a9d1c1f21a59c7ebba75f3eba190f6 | 1 | 0.030186 |
| .git/objects/16/tmp_obj_rHWkYQ | 3 | 0.090559 |
| .git/objects/17/5da15e5d09796b0fc212a02f7ca0bd58a44a3b | 1 | 0.030186 |
| .git/objects/17/tmp_obj_1JVAFI | 3 | 0.090559 |
| .git/objects/19/008baf2bba87fce9a3af2e5c19de3d1a4ae2c2 | 1 | 0.030186 |
| .git/objects/19/tmp_obj_eBEUgF | 3 | 0.090559 |
| .git/objects/1b/4206f6d29039aa2fb269966ee7a8502ea989c6 | 1 | 0.030186 |
| .git/objects/1b/tmp_obj_BXGr77 | 3 | 0.090559 |
| .git/objects/1d/705bfb2ad3020ea62ea38c40d44196da256490 | 1 | 0.030186 |
| .git/objects/1d/tmp_obj_Rp4V6l | 3 | 0.090559 |
| .git/objects/1f/f3cdf48a45bae04e63a56f2ea25bc7e8ee7f9d | 1 | 0.030186 |
| .git/objects/1f/tmp_obj_LDkSjr | 3 | 0.090559 |
| .git/objects/22/873236784f59d3501e8d70aa0d23afa26d699f | 5 | 0.150932 |
| .git/objects/23/67bdf4e254a5988f347d2a7776a09f7bfbf304 | 1 | 0.030186 |
| .git/objects/23/tmp_obj_UXIuo3 | 3 | 0.090559 |
| .git/objects/25/4319a57ad18c1406cecbbe06455ce4b7c7b4c3 | 1 | 0.030186 |
| .git/objects/25/8aa12a309133766fcdc37f525551ac16b01d2e | 1 | 0.030186 |
| .git/objects/25/tmp_obj_81zKfC | 3 | 0.090559 |
| .git/objects/25/tmp_obj_86AYe5 | 3 | 0.090559 |
| .git/objects/26/5dc6f9d3f456087fb5ded369efacf1614e2a5f | 1 | 0.030186 |
| .git/objects/26/da398574f1d6bcd4b27873a4cbccf26ef9850a | 1 | 0.030186 |
| .git/objects/26/tmp_obj_0VSyFt | 3 | 0.090559 |
| .git/objects/26/tmp_obj_lV6hxE | 3 | 0.090559 |
| .git/objects/28/65b7a073970706bd604cf9f05015bbc5c1a80d | 5 | 0.150932 |
| .git/objects/2c/3a6393932c2a5fe384ca3bbef4396744f9d0c4 | 1 | 0.030186 |
| .git/objects/2c/ebe12177217b13262765b436a364132cb955b0 | 1 | 0.030186 |
| .git/objects/2c/tmp_obj_OZG4Xk | 3 | 0.090559 |
| .git/objects/2c/tmp_obj_tUydoo | 3 | 0.090559 |
| .git/objects/2f/349de71bff4c3e92344fdd68f5c2c02768a39b | 5 | 0.150932 |
| .git/objects/32/5a79ec21b0dcead2bf9f21cde8379145eaa703 | 1 | 0.030186 |
| .git/objects/32/tmp_obj_67wBG0 | 3 | 0.090559 |
| .git/objects/37/3d1cbb6346effc994a229fe08ec4c4e249e598 | 1 | 0.030186 |
| .git/objects/37/74bd8186f45a5fb2d7241c669f824a1ddd3e41 | 1 | 0.030186 |
| .git/objects/37/tmp_obj_SpPfN7 | 3 | 0.090559 |
| .git/objects/37/tmp_obj_a6lQNe | 3 | 0.090559 |
| .git/objects/38/d9aec08d559add17b3b46f7989a76bdb0d5405 | 1 | 0.030186 |
| .git/objects/38/tmp_obj_H0Z8P8 | 3 | 0.090559 |
| .git/objects/3a/c2002fba2987065dedf42593e8506b555d0355 | 5 | 0.150932 |
| .git/objects/3b/841f1a40ec6341a7c40aeaf2121e4b082a7c5a | 1 | 0.030186 |
| .git/objects/3b/tmp_obj_VAvf04 | 3 | 0.090559 |
| .git/objects/3d/a4fd97cf77817f89403bbb2de305b6ba57691d | 1 | 0.030186 |
| .git/objects/3d/tmp_obj_fqDjJq | 3 | 0.090559 |
| .git/objects/40/194d9b3e31ff0d62fd2063beeb48180e38c92a | 1 | 0.030186 |
| .git/objects/40/85df3f52d85d200a5e4c296f88d071b3d12a4d | 5 | 0.150932 |
| .git/objects/40/tmp_obj_TX8O9j | 3 | 0.090559 |
| .git/objects/43/88f6b6bb2140cee741740f6898e76f685e05bc | 1 | 0.030186 |
| .git/objects/43/tmp_obj_bM0j40 | 3 | 0.090559 |
| .git/objects/44/60fdc6640bd62029272b51d468c5be8471dabf | 1 | 0.030186 |
| .git/objects/44/tmp_obj_rH759b | 3 | 0.090559 |
| .git/objects/46/01430fcd4fe2f97a28b6f23750b5cf030768fe | 1 | 0.030186 |
| .git/objects/46/6f91af0fe9b02bfea1d0b052278e656ae94167 | 1 | 0.030186 |
| .git/objects/46/d173d2b93567127b16ca09d65e0888cfc60b29 | 1 | 0.030186 |
| .git/objects/46/tmp_obj_IIjCFs | 3 | 0.090559 |
| .git/objects/46/tmp_obj_Ihfjri | 3 | 0.090559 |
| .git/objects/46/tmp_obj_rE7vlA | 3 | 0.090559 |
| .git/objects/47/7fc0befccc7137675b69998996c7c27d759f6f | 1 | 0.030186 |
| .git/objects/47/tmp_obj_yw3UYR | 3 | 0.090559 |
| .git/objects/4a/e654f79ef2c391fedf3584ce90952059b47aab | 1 | 0.030186 |
| .git/objects/4a/tmp_obj_AVUciM | 3 | 0.090559 |
| .git/objects/4b/74b1ca6a74c1beb5ae04dc7a5ce19d1fbad540 | 1 | 0.030186 |
| .git/objects/4b/tmp_obj_F3K7iF | 3 | 0.090559 |
| .git/objects/4d/077e85814a7f80e43c9845ec7979b21e026935 | 1 | 0.030186 |
| .git/objects/4d/tmp_obj_YCMaEo | 3 | 0.090559 |
| .git/objects/4f/aa5f461e79fdbc349915e2bbd9f005befb0ab9 | 1 | 0.030186 |
| .git/objects/4f/tmp_obj_PtlShj | 3 | 0.090559 |
| .git/objects/50/fd8c4b0d1dbb120aeae8c16d7c9904255f7d7a | 1 | 0.030186 |
| .git/objects/50/tmp_obj_ORkUm1 | 3 | 0.090559 |
| .git/objects/52/6936f58ad7d4eabe5e732c53676ef34c3cf401 | 1 | 0.030186 |
| .git/objects/52/c78063b0f6211a0c2a20d00b956857ec52720d | 1 | 0.030186 |
| .git/objects/52/tmp_obj_FIqGuw | 3 | 0.090559 |
| .git/objects/52/tmp_obj_tPVapb | 3 | 0.090559 |
| .git/objects/53/a5124306fb7548318978fa605964ebf30cc70a | 1 | 0.030186 |
| .git/objects/53/tmp_obj_107FVz | 3 | 0.090559 |
| .git/objects/55/d53969d06af760f875e5ea222d61586afac102 | 1 | 0.030186 |
| .git/objects/55/tmp_obj_lGjCMZ | 3 | 0.090559 |
| .git/objects/57/31eac57a363f52e3ef4adbf1a80364b74a8216 | 1 | 0.030186 |
| .git/objects/57/d331fdbcd436b5515a45a8d175e5d1a5f48b0a | 5 | 0.150932 |
| .git/objects/57/tmp_obj_nDKtWM | 3 | 0.090559 |
| .git/objects/58/88f4b3d777b0b064db4dd38512820f79e7fbe2 | 1 | 0.030186 |
| .git/objects/58/9a2b24f18c0b28d16167d76ee3b27d32f20ee7 | 5 | 0.150932 |
| .git/objects/58/tmp_obj_q1cjo9 | 3 | 0.090559 |
| .git/objects/59/7ca6a648068831298c97b2b33f8b041b973cd1 | 5 | 0.150932 |
| .git/objects/59/b677e795ee52d849e44a1aa201401fe0f89906 | 1 | 0.030186 |
| .git/objects/59/tmp_obj_XVkrir | 3 | 0.090559 |
| .git/objects/5a/46b39c7af2989cd09e8dfb841ac755b12a1c91 | 1 | 0.030186 |
| .git/objects/5a/tmp_obj_oIM9AH | 3 | 0.090559 |
| .git/objects/5e/74ca396351b35da55911b59aa249ff09f27631 | 1 | 0.030186 |
| .git/objects/5e/tmp_obj_a0k9Vl | 3 | 0.090559 |
| .git/objects/60/eaa45e9b21302674ea30035d6b17be479a320e | 1 | 0.030186 |
| .git/objects/60/tmp_obj_d4rkZ7 | 3 | 0.090559 |
| .git/objects/62/7f5d9b4a68729ea7ef48d4219683fe17ce7e19 | 1 | 0.030186 |
| .git/objects/62/tmp_obj_Wgo29v | 3 | 0.090559 |
| .git/objects/65/8d2fc993a7f497047190bd66a5a3134e323fc1 | 1 | 0.030186 |
| .git/objects/65/c2d25b0c539af5f18585ff9f33646823fd3812 | 1 | 0.030186 |
| .git/objects/65/tmp_obj_5bvcg1 | 3 | 0.090559 |
| .git/objects/65/tmp_obj_9jX2qC | 3 | 0.090559 |
| .git/objects/67/09a605b3c8abbaf426fb0e1f2700717cc09236 | 1 | 0.030186 |
| .git/objects/67/4f2c4d52cef3f7652c4aa1ab19f9d18002cd71 | 1 | 0.030186 |
| .git/objects/67/tmp_obj_HNb8g0 | 3 | 0.090559 |
| .git/objects/67/tmp_obj_p7NjKZ | 3 | 0.090559 |
| .git/objects/6a/4bf21a842379be98e3ac6389d2b591fa5ffca3 | 1 | 0.030186 |
| .git/objects/6a/tmp_obj_4wDk1s | 3 | 0.090559 |
| .git/objects/6c/4e70660fb9e816b6f42b75b867235762b22f7c | 1 | 0.030186 |
| .git/objects/6c/e3456b31741f8da9559b99cef7be38ea74140d | 5 | 0.150932 |
| .git/objects/6c/tmp_obj_4b0JZj | 3 | 0.090559 |
| .git/objects/6d/46de2bfd4e831ae655ff917131ec6b5252da61 | 1 | 0.030186 |
| .git/objects/6d/638d44ee86f08d6d0a7f71323b1eac5af4e8cd | 5 | 0.150932 |
| .git/objects/6d/b830f3d8147857be6bceed74398ef9ab5655e2 | 1 | 0.030186 |
| .git/objects/6d/tmp_obj_oJnqBy | 3 | 0.090559 |
| .git/objects/6d/tmp_obj_ySvcc9 | 3 | 0.090559 |
| .git/objects/70/e9e081ff374635b8393119b4ffe860f8458e6a | 1 | 0.030186 |
| .git/objects/70/tmp_obj_cKNWQZ | 3 | 0.090559 |
| .git/objects/73/372cd67884b7d20540632d3898331fd5baf829 | 1 | 0.030186 |
| .git/objects/73/tmp_obj_IKh5DV | 3 | 0.090559 |
| .git/objects/77/69fb734fe9f4fa8a729565447da8d798f9df2b | 1 | 0.030186 |
| .git/objects/77/tmp_obj_jUaEL1 | 3 | 0.090559 |
| .git/objects/84/fbca7b4a6fc6a7f2cb67c8a10c4ede0fdb34c3 | 1 | 0.030186 |
| .git/objects/84/tmp_obj_Taq1Ei | 3 | 0.090559 |
| .git/objects/86/7ab1147419b5c8aeaa13c36864f07dda7f0246 | 1 | 0.030186 |
| .git/objects/86/tmp_obj_GgAes0 | 3 | 0.090559 |
| .git/objects/8d/59a63528dc1619ae21e72e27be398b763a1e5c | 1 | 0.030186 |
| .git/objects/8d/tmp_obj_yydE2S | 3 | 0.090559 |
| .git/objects/8f/590de12325aac92437634a6ee09f901f0dcefd | 1 | 0.030186 |
| .git/objects/8f/86454a26f1737f2b302d4c24e728de10011d73 | 1 | 0.030186 |
| .git/objects/8f/tmp_obj_Hi7SVa | 3 | 0.090559 |
| .git/objects/8f/tmp_obj_zvBDDa | 3 | 0.090559 |
| .git/objects/91/e5c7a2a64ec812367f01b3efbb04a799a07ab4 | 1 | 0.030186 |
| .git/objects/91/tmp_obj_VUDfMo | 3 | 0.090559 |
| .git/objects/96/daa7bccacb13cd28858615ce65f358a699c813 | 1 | 0.030186 |
| .git/objects/96/tmp_obj_tqWAmm | 3 | 0.090559 |
| .git/objects/97/a0b46388a227f6641efa3709ece42a8fcbffdd | 1 | 0.030186 |
| .git/objects/97/tmp_obj_c2mzJ0 | 3 | 0.090559 |
| .git/objects/98/4bed1a10d43607613158f61a7e8c6edb7902a5 | 1 | 0.030186 |
| .git/objects/98/tmp_obj_WuS9iG | 3 | 0.090559 |
| .git/objects/9c/422a27e99dd662712a708885998d8740e207c1 | 1 | 0.030186 |
| .git/objects/9c/tmp_obj_Vbq1N5 | 3 | 0.090559 |
| .git/objects/9e/6592ff03f3ad67af2d8915165539767a0c3150 | 1 | 0.030186 |
| .git/objects/9e/a5c5edc7fe046f11f242cf80b7828dee4e0604 | 1 | 0.030186 |
| .git/objects/9e/tmp_obj_JxVcZR | 3 | 0.090559 |
| .git/objects/9e/tmp_obj_uJRBbi | 3 | 0.090559 |
| .git/objects/a2/079c3183dcc60395595b9ea91f2326e7cd67e0 | 1 | 0.030186 |
| .git/objects/a2/acbdaf73416ad7624a2bd7e9a2fd87c53d1a65 | 1 | 0.030186 |
| .git/objects/a2/tmp_obj_vbXcXX | 3 | 0.090559 |
| .git/objects/a2/tmp_obj_xnlliH | 3 | 0.090559 |
| .git/objects/a3/cf93ba94b64573af41404938dc4bce017719a4 | 1 | 0.030186 |
| .git/objects/a3/d71367fb8c5b802cbb4d542958da965df41651 | 1 | 0.030186 |
| .git/objects/a3/tmp_obj_KIU1pW | 3 | 0.090559 |
| .git/objects/a3/tmp_obj_OhPlvw | 3 | 0.090559 |
| .git/objects/a6/887195e3a5e7bc11b03c52edba60342da22bbf | 5 | 0.150932 |
| .git/objects/a9/2166aa255f55dd0f9fd5d11dab588d853a7cc2 | 1 | 0.030186 |
| .git/objects/a9/tmp_obj_O7nEw8 | 3 | 0.090559 |
| .git/objects/ab/ab360d1a09b8925307c1be289aee8934d74117 | 5 | 0.150932 |
| .git/objects/ad/608ad8cefe09b8bd5d31b32154e8309724b259 | 1 | 0.030186 |
| .git/objects/ad/tmp_obj_ZbTcfY | 3 | 0.090559 |
| .git/objects/b0/a2e28cd020d27ab164568983fc7f51493acbd8 | 1 | 0.030186 |
| .git/objects/b0/tmp_obj_KAOjfI | 3 | 0.090559 |
| .git/objects/b1/53da7191cbcdc7a701318735b2b687ee4166ae | 1 | 0.030186 |
| .git/objects/b1/tmp_obj_jytuk2 | 3 | 0.090559 |
| .git/objects/b2/4c50b54facb7ee67cbef22243e50a31116f47d | 1 | 0.030186 |
| .git/objects/b2/a5d6fee12e542189bc85a771b31b65783112c7 | 1 | 0.030186 |
| .git/objects/b2/tmp_obj_8q7wGs | 3 | 0.090559 |
| .git/objects/b2/tmp_obj_HnLhdi | 3 | 0.090559 |
| .git/objects/b3/142ea7bc3617b9812cea8ed39ba9cbc3791ae2 | 1 | 0.030186 |
| .git/objects/b3/38aaf3104495c2ca0e5da94f4dae1ecca5a26b | 1 | 0.030186 |
| .git/objects/b3/c98f62a1d3509f6ce571544b45f4eaa37e3b21 | 1 | 0.030186 |
| .git/objects/b3/tmp_obj_mLlKVS | 3 | 0.090559 |
| .git/objects/b3/tmp_obj_sy4VdY | 3 | 0.090559 |
| .git/objects/b3/tmp_obj_v9iKlE | 3 | 0.090559 |
| .git/objects/b5/181ca225271616291c1059794ace51a592d576 | 1 | 0.030186 |
| .git/objects/b5/tmp_obj_z5nsyb | 3 | 0.090559 |
| .git/objects/b7/4ad08e072c7ff46eb4852cdc8d5fbae7e173d8 | 1 | 0.030186 |
| .git/objects/b7/tmp_obj_stWYao | 3 | 0.090559 |
| .git/objects/bc/65116759301fe5c51dbd0fe54d2394769c8ce1 | 1 | 0.030186 |
| .git/objects/bc/tmp_obj_KJQ0nD | 3 | 0.090559 |
| .git/objects/be/9a1ed2ecfae448acc67ba7e456eb2e7234d44d | 1 | 0.030186 |
| .git/objects/be/tmp_obj_fQs7mP | 3 | 0.090559 |
| .git/objects/c5/31fd7eb44471e50acadf7da7e98457719008e2 | 1 | 0.030186 |
| .git/objects/c5/6f348930191ae6dfc92e6894caf511e6fdb3c8 | 1 | 0.030186 |
| .git/objects/c5/889c2b1f99eacb847a3ec7a72b4cc3060861bc | 1 | 0.030186 |
| .git/objects/c5/9562ca4798b9216de4909954c485b6eab32e1c | 1 | 0.030186 |
| .git/objects/c5/b166dc5f95d2c49d80aaf740edb8e9f59450a7 | 1 | 0.030186 |
| .git/objects/c5/ff0fbb7dc5d96cae6b25fbb307c58da8c2118e | 1 | 0.030186 |
| .git/objects/c5/tmp_obj_K4gVOx | 3 | 0.090559 |
| .git/objects/c5/tmp_obj_LbehJ1 | 3 | 0.090559 |
| .git/objects/c5/tmp_obj_VcmbUs | 3 | 0.090559 |
| .git/objects/c5/tmp_obj_Vo5WXn | 3 | 0.090559 |
| .git/objects/c5/tmp_obj_c3nyg4 | 3 | 0.090559 |
| .git/objects/c5/tmp_obj_ppUOBM | 3 | 0.090559 |
| .git/objects/c6/bc2e98916d02697fe2cf5393728ba8fd713798 | 1 | 0.030186 |
| .git/objects/c6/tmp_obj_1IAO1z | 3 | 0.090559 |
| .git/objects/c8/a62b55744699d1b607ed4e2a5a87cddfc829a9 | 5 | 0.150932 |
| .git/objects/c8/ff82297d0375fa69805f1411e5f09d87f9af70 | 1 | 0.030186 |
| .git/objects/c8/tmp_obj_t0wSj0 | 3 | 0.090559 |
| .git/objects/ca/1cadd7da50326a736204d712a8d199c1fa225c | 1 | 0.030186 |
| .git/objects/ca/tmp_obj_0gySe3 | 3 | 0.090559 |
| .git/objects/ce/2b3c6dc482d34740b382d1164836c2a634b048 | 1 | 0.030186 |
| .git/objects/ce/tmp_obj_xkavtK | 3 | 0.090559 |
| .git/objects/d0/63e4a1730a5423c34a8068df4cf3417b532d55 | 1 | 0.030186 |
| .git/objects/d0/65b0244535c5101a7c11b80913a0eacb42aa30 | 5 | 0.150932 |
| .git/objects/d0/b816d77be3d288150c49866939f6b08badc5ef | 1 | 0.030186 |
| .git/objects/d0/tmp_obj_WwpREx | 3 | 0.090559 |
| .git/objects/d0/tmp_obj_zt5Ivx | 3 | 0.090559 |
| .git/objects/d1/6dd206096c2965e3fe33972c8eac9a34bf9ba4 | 1 | 0.030186 |
| .git/objects/d1/tmp_obj_lnzjhe | 3 | 0.090559 |
| .git/objects/d4/dd8a396ac54141debfd633634f2f1d6fe598ff | 1 | 0.030186 |
| .git/objects/d4/tmp_obj_uVHjOQ | 3 | 0.090559 |
| .git/objects/d6/010a09f5b13a41cd46e1b6bd0b1da19286bef6 | 5 | 0.150932 |
| .git/objects/d6/a32095f4019dab181336ffc2ba9dba383f17eb | 1 | 0.030186 |
| .git/objects/d6/tmp_obj_cuwf7B | 3 | 0.090559 |
| .git/objects/d7/b03e8c3afab9e0ecef790fc75d23aa2e36ab7b | 1 | 0.030186 |
| .git/objects/d7/tmp_obj_xrpYmY | 3 | 0.090559 |
| .git/objects/d8/d4f7d47152d6da90a2b615d287536265f66f92 | 1 | 0.030186 |
| .git/objects/d8/tmp_obj_cujCJm | 3 | 0.090559 |
| .git/objects/da/3b6fad9cb8e0bac77e05fb0cff1fb1aa3ab781 | 1 | 0.030186 |
| .git/objects/da/a280bddf704d07a1fb77d59a176ff0238df538 | 5 | 0.150932 |
| .git/objects/da/tmp_obj_OJvw83 | 3 | 0.090559 |
| .git/objects/dd/3dccf2248adc8f5ed20f00e22703828b92848c | 1 | 0.030186 |
| .git/objects/dd/dc3c745e273de7eeb265ffc688bd74719fff73 | 1 | 0.030186 |
| .git/objects/dd/tmp_obj_hAQmSW | 3 | 0.090559 |
| .git/objects/dd/tmp_obj_tGSIPl | 3 | 0.090559 |
| .git/objects/de/5de65ad20a15076f33673ab4ab4d6c52c3d6f9 | 1 | 0.030186 |
| .git/objects/de/tmp_obj_9tfP6R | 3 | 0.090559 |
| .git/objects/e1/ab10d48f3956d142af11641726a00ca9f08244 | 1 | 0.030186 |
| .git/objects/e1/tmp_obj_tenuew | 3 | 0.090559 |
| .git/objects/e3/071390b4f2fa43bea2b7299fa1842af4ec0abd | 1 | 0.030186 |
| .git/objects/e3/112108dcb57e696ff04e100d060e1d84ec9e24 | 1 | 0.030186 |
| .git/objects/e3/tmp_obj_RWqH5N | 3 | 0.090559 |
| .git/objects/e3/tmp_obj_VUgPb8 | 3 | 0.090559 |
| .git/objects/e5/edc77d7d9d744e9cc45021f048a34bbebf6b6a | 1 | 0.030186 |
| .git/objects/e5/tmp_obj_whHQOM | 3 | 0.090559 |
| .git/objects/ef/00bb39df37f81bad2ac04bc57ea9f59a684882 | 1 | 0.030186 |
| .git/objects/ef/tmp_obj_wgGDY5 | 3 | 0.090559 |
| .git/objects/f0/f12b903130e7e3b0b3a86d9bc5fcc0c8a57707 | 1 | 0.030186 |
| .git/objects/f0/tmp_obj_ZN1QCg | 3 | 0.090559 |
| .git/objects/f1/506574194ae72e1fbf136db6ca065630712d68 | 1 | 0.030186 |
| .git/objects/f1/tmp_obj_eDgwu2 | 3 | 0.090559 |
| .git/objects/f3/3c323e66536e38e44911c509623e001ab964a2 | 1 | 0.030186 |
| .git/objects/f3/tmp_obj_SK9k5S | 3 | 0.090559 |
| .git/objects/f4/c0c912d2fce9dcdbaeaf5dc61484673804ab18 | 5 | 0.150932 |
| .git/objects/f5/a8a02b6b1a0d291cf44abea3c6643f9ae9bb87 | 1 | 0.030186 |
| .git/objects/f5/tmp_obj_F7HUL9 | 3 | 0.090559 |
| .git/objects/f6/bf873770f3a15d14e491ba89e6a4c5f7ff8eba | 1 | 0.030186 |
| .git/objects/f6/tmp_obj_U2R9jV | 3 | 0.090559 |
| .git/objects/f8/af985435701b02b3539c1cd6ec72b2940eb313 | 1 | 0.030186 |
| .git/objects/f8/tmp_obj_NmEAF7 | 3 | 0.090559 |
| .git/objects/fd/bfb37bea3fdcb51802ab2e50b462fe564a8eb2 | 1 | 0.030186 |
| .git/objects/fd/tmp_obj_5Jvu5m | 3 | 0.090559 |
| .git/objects/ff/be07c1aa57cd1d11f23de972047ab36c0f32fd | 1 | 0.030186 |
| .git/objects/ff/tmp_obj_YbtYIo | 3 | 0.090559 |
| .git/objects/pack/pack-fc01b4cdfb0799a11fabe55de4a9e13605f559ad.pack | 1 | 0.030186 |
| .git/refs/remotes/origin/HEAD.lock | 42 | 1.267829 |
| .git/refs/t3/orchestration-v2/checkpoints/MDIzZGU4YzUzY2RmN2U4ZGMzMzAwODMyMjA2ODI5OWI/ordinal/52 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MDIzZGU4YzUzY2RmN2U4ZGMzMzAwODMyMjA2ODI5OWI/ordinal/52.lock | 3 | 0.090559 |
| .git/refs/t3/orchestration-v2/checkpoints/MWNmNGZjMmU4MGEwNWYzYzc1YzRjZTkzNzI5NGFiMjQ | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWNmNGZjMmU4MGEwNWYzYzc1YzRjZTkzNzI5NGFiMjQ/ordinal/0 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWNmNGZjMmU4MGEwNWYzYzc1YzRjZTkzNzI5NGFiMjQ/ordinal/0.lock | 2 | 0.060373 |
| .git/refs/t3/orchestration-v2/checkpoints/MWNmNGZjMmU4MGEwNWYzYzc1YzRjZTkzNzI5NGFiMjQ/ordinal/1 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWNmNGZjMmU4MGEwNWYzYzc1YzRjZTkzNzI5NGFiMjQ/ordinal/1.lock | 3 | 0.090559 |
| .git/refs/t3/orchestration-v2/checkpoints/MWQ2YTMxMjZkNTM3NmVmY2VmYjQyYWJlNjYxZDA5OTQ | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWQ2YTMxMjZkNTM3NmVmY2VmYjQyYWJlNjYxZDA5OTQ/ordinal/0 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWQ2YTMxMjZkNTM3NmVmY2VmYjQyYWJlNjYxZDA5OTQ/ordinal/0.lock | 2 | 0.060373 |
| .git/refs/t3/orchestration-v2/checkpoints/MWQ2YTMxMjZkNTM3NmVmY2VmYjQyYWJlNjYxZDA5OTQ/ordinal/1 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/MWQ2YTMxMjZkNTM3NmVmY2VmYjQyYWJlNjYxZDA5OTQ/ordinal/1.lock | 3 | 0.090559 |
| .git/refs/t3/orchestration-v2/checkpoints/NDFkMDM5MDBmMmI1M2QxNTQzMzU2ZjM3OTRkNDJhZjg/ordinal/5 | 1 | 0.030186 |
| .git/refs/t3/orchestration-v2/checkpoints/NDFkMDM5MDBmMmI1M2QxNTQzMzU2ZjM3OTRkNDJhZjg/ordinal/5.lock | 3 | 0.090559 |
| .git/t3-checkpoint-index-200f3de5-6256-4a91-a3d0-123edb76d9d6 | 8 | 0.241491 |
| .git/t3-checkpoint-index-200f3de5-6256-4a91-a3d0-123edb76d9d6.lock | 9 | 0.271678 |
| .git/t3-checkpoint-index-39008882-6c0e-4085-949b-0ba5f8eb5d42 | 9 | 0.271678 |
| .git/t3-checkpoint-index-39008882-6c0e-4085-949b-0ba5f8eb5d42.lock | 9 | 0.271678 |
| .git/t3-checkpoint-index-4fd0962f-20fe-4df5-a97d-bb036eb7ce05 | 9 | 0.271678 |
| .git/t3-checkpoint-index-4fd0962f-20fe-4df5-a97d-bb036eb7ce05.lock | 9 | 0.271678 |
| .git/t3-checkpoint-index-51e8b3a0-0b5d-4329-a50f-592ab98bc2e8 | 10 | 0.301864 |
| .git/t3-checkpoint-index-51e8b3a0-0b5d-4329-a50f-592ab98bc2e8.lock | 9 | 0.271678 |
| .git/t3-checkpoint-index-8047d297-3169-4023-832d-24924dff9131 | 9 | 0.271678 |
| .git/t3-checkpoint-index-8047d297-3169-4023-832d-24924dff9131.lock | 9 | 0.271678 |
| .git/t3-checkpoint-index-9bc208a6-6cc5-4043-824b-0a8bc8e23cb8 | 9 | 0.271678 |
| .git/t3-checkpoint-index-9bc208a6-6cc5-4043-824b-0a8bc8e23cb8.lock | 9 | 0.271678 |
| .git/worktrees/t3-12f724c7/index.lock | 6 | 0.181118 |

## 噪声生成器校准

| 参数 | 校准前 | 校准后 | 实测操作数 | 记录覆盖秒 |
| --- | --- | --- | --- | --- |
| git status 周期 s | 1 | 1.000000 | 不可从只读调用推出 | 3628.276582 |
| cargo check 次/启动 | 1 | 1 | target 汇总 0 行 | 3628.276582 |
| index_refresh 周期 s | 未配置 | 3628.276582 | 1 | 3628.276582 |
| fetch_head 周期 s | 未配置 | 46.516366 | 78 | 3628.276582 |
| ephemeral_dirs 周期 s | 未配置 | 117.041180 | 31 | 3628.276582 |
| checkpoint 周期 s | 未配置 | 453.534573 | 8 | 3628.276582 |
| config_lock 周期 s | 未配置 | 907.069146 | 4 | 3628.276582 |
| packed_refs_lock 周期 s | 未配置 | 1209.425527 | 3 | 3628.276582 |
| remote_head_lock 周期 s | 未配置 | 93.032733 | 39 | 3628.276582 |
| worktree_index 周期 s | 未配置 | 725.655316 | 5 | 3628.276582 |
| source_touch 周期 s | 5 | — | 0 | 3628.276582 |
| 其他 Agent 沙箱目录寿命 s | 未配置 | 0.100000 | 实测小于 1 秒 | 3628.276582 |
| 噪声窗口 s | 未规定 | 30 | 每次实际运行时长见下一表 | 3628.276582 |
| 初始相位 | 未规定 | seeded-uniform | 按运行种子均匀抽取 | 3628.276582 |

## 噪声实际产生记录

| 运行 | 配置来源 | 实际时长 s | 各类事件数 | cargo check code | 噪声进程 code |
| --- | --- | --- | --- | --- | --- |
| [S2-noise-P1-1-1794497694692569642](results/S2-noise-P1-1-1794497694692569642.json.gz) | results/calibration-1794497662850941537 | 40.916780 | {"git_status":41,"fetch_head":1,"remote_head_lock":1} | 0 | 0 |
| [S2-noise-P2-1-1794497736792212675](results/S2-noise-P2-1-1794497736792212675.json.gz) | results/calibration-1794497662850941537 | 34.039825 | {"git_status":35,"fetch_head":1} | 0 | 0 |
| [S2-noise-P3-1-1794497771991094191](results/S2-noise-P3-1-1794497771991094191.json.gz) | results/calibration-1794497662850941537 | 34.028242 | {"git_status":35,"packed_refs_lock":1,"remote_head_lock":1,"fetch_head":1} | 0 | 0 |
| [S2-noise-P1-2-1794497807017717004](results/S2-noise-P1-2-1794497807017717004.json.gz) | results/calibration-1794497662850941537 | 33.626910 | {"git_status":34,"fetch_head":1} | 0 | 0 |
| [S2-noise-P2-2-1794497841655067685](results/S2-noise-P2-2-1794497841655067685.json.gz) | results/calibration-1794497662850941537 | 33.864419 | {"git_status":34,"fetch_head":1,"remote_head_lock":1} | 0 | 0 |
| [S2-noise-P3-2-1794497876519469892](results/S2-noise-P3-2-1794497876519469892.json.gz) | results/calibration-1794497662850941537 | 34.174643 | {"git_status":35,"ephemeral_dirs":1,"remote_head_lock":1,"fetch_head":1} | 0 | 0 |
| [S2-noise-P1-3-1794497911703667326](results/S2-noise-P1-3-1794497911703667326.json.gz) | results/calibration-1794497662850941537 | 33.880712 | {"git_status":34,"remote_head_lock":1} | 0 | 0 |
| [S2-noise-P2-3-1794497946579129582](results/S2-noise-P2-3-1794497946579129582.json.gz) | results/calibration-1794497662850941537 | 34.030299 | {"git_status":35,"fetch_head":1} | 0 | 0 |
| [S2-noise-P3-3-1794497981605125568](results/S2-noise-P3-3-1794497981605125568.json.gz) | results/calibration-1794497662850941537 | 33.672373 | {"git_status":34,"ephemeral_dirs":1,"fetch_head":1,"remote_head_lock":1} | 0 | 0 |
| [S6-noise-commit-P1-1-1794498253167465036](results/S6-noise-commit-P1-1-1794498253167465036.json.gz) | results/calibration-1794497662850941537 | 34.170652 | {"git_status":35,"fetch_head":1} | 0 | 0 |
| [S6-noise-commit-P1-2-1794498290424485545](results/S6-noise-commit-P1-2-1794498290424485545.json.gz) | results/calibration-1794497662850941537 | 34.453536 | {"git_status":35,"fetch_head":1} | 0 | 0 |
| [S6-noise-commit-P1-3-1794498327952897256](results/S6-noise-commit-P1-3-1794498327952897256.json.gz) | results/calibration-1794497662850941537 | 34.391666 | {"git_status":35,"worktree_index":1} | 0 | 0 |
| [S6-noise-discard-P1-1-1794498365436988018](results/S6-noise-discard-P1-1-1794498365436988018.json.gz) | results/calibration-1794497662850941537 | 34.631269 | {"git_status":35,"config_lock":1,"fetch_head":1,"ephemeral_dirs":1} | 0 | 0 |
| [S6-noise-discard-P1-2-1794498403164302922](results/S6-noise-discard-P1-2-1794498403164302922.json.gz) | results/calibration-1794497662850941537 | 34.628911 | {"git_status":35,"ephemeral_dirs":1,"fetch_head":1} | 0 | 0 |
| [S6-noise-discard-P1-3-1794498440902020037](results/S6-noise-discard-P1-3-1794498440902020037.json.gz) | results/calibration-1794497662850941537 | 34.586587 | {"git_status":35,"fetch_head":1} | 0 | 0 |
| [S6-noise-commit-P2-1-1794498478607137203](results/S6-noise-commit-P2-1-1794498478607137203.json.gz) | results/calibration-1794497662850941537 | 32.559930 | {"git_status":33,"fetch_head":1} | 0 | 0 |
| [S6-noise-commit-P2-2-1794498514290246101](results/S6-noise-commit-P2-2-1794498514290246101.json.gz) | results/calibration-1794497662850941537 | 32.430434 | {"git_status":33,"worktree_index":1} | 0 | 0 |
| [S6-noise-commit-P2-3-1794498549818752085](results/S6-noise-commit-P2-3-1794498549818752085.json.gz) | results/calibration-1794497662850941537 | 33.011631 | {"git_status":33,"fetch_head":1} | 0 | 0 |
| [S6-noise-discard-P2-1-1794498585987151690](results/S6-noise-discard-P2-1-1794498585987151690.json.gz) | results/calibration-1794497662850941537 | 32.895596 | {"git_status":33,"config_lock":1} | 0 | 0 |
| [S6-noise-discard-P2-2-1794498622078993137](results/S6-noise-discard-P2-2-1794498622078993137.json.gz) | results/calibration-1794497662850941537 | 32.429615 | {"git_status":33,"remote_head_lock":1} | 0 | 0 |
| [S6-noise-discard-P2-3-1794498657644298339](results/S6-noise-discard-P2-3-1794498657644298339.json.gz) | results/calibration-1794497662850941537 | 32.390230 | {"git_status":33,"ephemeral_dirs":1} | 0 | 0 |
| [S6-noise-commit-P3-1-1794498693155982184](results/S6-noise-commit-P3-1-1794498693155982184.json.gz) | results/calibration-1794497662850941537 | 33.581745 | {"git_status":34,"fetch_head":1} | 0 | 0 |
| [S6-noise-commit-P3-2-1794498729870037822](results/S6-noise-commit-P3-2-1794498729870037822.json.gz) | results/calibration-1794497662850941537 | 33.941994 | {"git_status":34} | 0 | 0 |
| [S6-noise-commit-P3-3-1794498766967584778](results/S6-noise-commit-P3-3-1794498766967584778.json.gz) | results/calibration-1794497662850941537 | 33.999138 | {"git_status":34,"remote_head_lock":1,"fetch_head":1} | 0 | 0 |
| [S6-noise-discard-P3-1-1794498804132449318](results/S6-noise-discard-P3-1-1794498804132449318.json.gz) | results/calibration-1794497662850941537 | 33.418282 | {"git_status":34,"fetch_head":1} | 0 | 0 |
| [S6-noise-discard-P3-2-1794498840808183303](results/S6-noise-discard-P3-2-1794498840808183303.json.gz) | results/calibration-1794497662850941537 | 34.045024 | {"git_status":35,"config_lock":1,"fetch_head":1} | 0 | 0 |
| [S6-noise-discard-P3-3-1794498877943600235](results/S6-noise-discard-P3-3-1794498877943600235.json.gz) | results/calibration-1794497662850941537 | 33.782775 | {"git_status":34,"packed_refs_lock":1,"fetch_head":1,"ephemeral_dirs":1} | 0 | 0 |

## 原型与方案的偏差

| 项 | 实际执行 |
| --- | --- |
| 分支 / 范围 | 按覆盖指令使用 exp/overlay-commit；Python 原型，无新增 Cargo.toml；未修改 docs/ |
| S6 提交入口 | bot-run git commit -m t；脚本其余 Git 命令保持方案；set -e 遇非零退出停止 |
| S6 首批前置失败 | 本地 clone origin 使 bot-run 拒绝；原始失败保留，修正实验 repo/baseline 的 origin 到 https://github.com/Skyline10124/lattice.git 后重新运行；未推送 LATTICE |
| 连续失败处理偏差 | 首批只将 namespace blocked 视为基础设施失败，bot-run 拒绝曾连续执行 3 次；现识别 bot-run 前置拒绝并在连续两次后跳过 |
| S6 初始改动 | 脚本前在实验克隆 README 追加 S6 input，确保 commit 非空 |
| S6 丢弃 | 新增 --finish discard，对成功脚本丢弃草稿层；P3 真实 Git 写入按实记录 |
| S9 时序 | 双 overlay 同时运行并产生草稿；同步点控制 A 完成提交后 B 检查；未测双方同时进入预检-写回区间 |
| C1 | 快照记录路径、size、mtime_ns、ctime_ns、inode，另保留 mode；含/不含 target 分别计时 |
| 目录 | 对删除目录的后代也预检、留前态；只对 opaque=y 清理下层子项 |
| T0 | 取实验文件系统中新建文件 ctime_ns；不使用系统时钟作为冲突基准 |
| S5 产物计数 | target 字节为上层常规文件逻辑大小之和；非实际磁盘占用；每次恢复相同预热基线 |
| 噪声频率 | inotify 不提供只读 git status 次数、无写入 cargo check 次数；这两项保留原方案 |
| 噪声新增类型 | 其他 Agent 沙箱目录、FETCH_HEAD、配置/引用锁、独立检查点索引/对象/引用、工作树索引 |
| 噪声动作映射 | FETCH_HEAD 原值写回；Git 锁创建后删除；工作树事件使用独立 index；检查点生成新的有效树引用；未重放真实对象内容或网络 fetch |
| 噪声时间窗口 | 30 秒 + 脚本/核查收尾；随机初相位；低频动作可能未在每个窗口出现 |
| 崩溃与并发原子性 | 无 WAL/崩溃回滚；冲突预检后无跨写者锁；脚本后的逐文件 rename 不是跨文件原子提交 |
| EXDEV 和预置失败 | S7 EXDEV、S5 测试失败和 P2 只读拒绝作为场景结局记录；不计为基础设施阻塞 |

## 中断恢复

| 项 | 原始值 |
| --- | --- |
| 完整运行数 | 39 |
| 不完整运行 | [] |
| 实验 overlay 挂载 | [] |
| 恢复前 status |  M README.md<br> |
| 恢复后 status |  |
| 恢复后 fsck code | 0 |
| fsck 与克隆基线 stdout 相同 | True |
| S9 | 6 组 / 12 份 JSON 均完整；删除不完整记录 0；重跑 0 |

## 验证

| 记录 | 命令 | 退出码 | 秒 | 结果原文/字段 |
| --- | --- | --- | --- | --- |
| tests-red.json | ["unshare","--user","--map-root-user","--mount","python3","-m","unittest","-v","test_commit"] | 1 | — | test_commit (unittest.loader._FailedTest.test_commit) ... ERROR<br><br>======================================================================<br>ERROR: test_commit (unittest.loader._FailedTest.test_commit)<br>----------------------------------------------------------------------<br>ImportError: Failed to import test module: test_commit<br>Traceback (most recent call last):<br>  File "/usr/lib64/python3.12/unittest/loader.py", line 137, in loadTestsFromName<br>    module = __import__(module_name)<br>             ^^^^^^^^^^^^^^^^^^^^^^^<br>  File "/home/gcp-vegas/.t3/worktrees/lancet/t3-dc7a648f/prototypes/overlay-commit/test_commit.py", line 10, in <module><br>    from commit import classify, commit, snapshot, fs_time<br>ModuleNotFoundError: No module named 'commit'<br><br><br>----------------------------------------------------------------------<br>Ran 1 test in 0.000s<br><br>FAILED (errors=1)<br> |
| tests-first-implementation.json | ["unshare","--user","--map-root-user","--mount","python3","-m","unittest","-v","test_commit"] | 1 | 0.278392 | test_any_conflict_prevents_all_writes (test_commit.CommitTests.test_any_conflict_prevents_all_writes) ... ok<br>test_opaque_directory (test_commit.CommitTests.test_opaque_directory) ... ok<br>test_redirect_is_recorded_without_processing_children (test_commit.CommitTests.test_redirect_is_recorded_without_processing_children) ... ok<br>test_strip_all_overlay_xattrs_and_symlink (test_commit.CommitTests.test_strip_all_overlay_xattrs_and_symlink) ... FAIL<br>test_target_has_no_conflict_check_or_preimage (test_commit.CommitTests.test_target_has_no_conflict_check_or_preimage) ... ok<br>test_timestamp_only_and_permission_change (test_commit.CommitTests.test_timestamp_only_and_permission_change) ... ok<br>test_whiteout_character_zero_zero (test_commit.CommitTests.test_whiteout_character_zero_zero) ... ok<br><br>======================================================================<br>FAIL: test_strip_all_overlay_xattrs_and_symlink (test_commit.CommitTests.test_strip_all_overlay_xattrs_and_symlink)<br>----------------------------------------------------------------------<br>Traceback (most recent call last):<br>  File "/home/gcp-vegas/.t3/worktrees/lancet/t3-dc7a648f/prototypes/overlay-commit/test_commit.py", line 79, in test_strip_all_overlay_xattrs_and_symlink<br>    self.assertEqual(os.listxattr(self.repo / 'a'), [])<br>AssertionError: Lists differ: ['security.selinux'] != []<br><br>First list contains 1 additional elements.<br>First extra element 0:<br>'security.selinux'<br><br>- ['security.selinux']<br>+ []<br><br>----------------------------------------------------------------------<br>Ran 7 tests in 0.218s<br><br>FAILED (failures=1)<br> |
| tests-green.json | ["unshare","--user","--map-root-user","--mount","python3","-m","unittest","-v","test_commit"] | 0 | 0.121857 | test_any_conflict_prevents_all_writes (test_commit.CommitTests.test_any_conflict_prevents_all_writes) ... ok<br>test_opaque_directory (test_commit.CommitTests.test_opaque_directory) ... ok<br>test_redirect_is_recorded_without_processing_children (test_commit.CommitTests.test_redirect_is_recorded_without_processing_children) ... ok<br>test_strip_all_overlay_xattrs_and_symlink (test_commit.CommitTests.test_strip_all_overlay_xattrs_and_symlink) ... ok<br>test_target_has_no_conflict_check_or_preimage (test_commit.CommitTests.test_target_has_no_conflict_check_or_preimage) ... ok<br>test_timestamp_only_and_permission_change (test_commit.CommitTests.test_timestamp_only_and_permission_change) ... ok<br>test_whiteout_character_zero_zero (test_commit.CommitTests.test_whiteout_character_zero_zero) ... ok<br><br>----------------------------------------------------------------------<br>Ran 7 tests in 0.069s<br><br>OK<br> |
| syntax-check.json | ["python3","-m","py_compile","prototypes/overlay-commit/calibrate.py","prototypes/overlay-commit/cleanup.py","prototypes/overlay-commit/commit.py","prototypes/overlay-commit/noise.py","prototypes/overlay-commit/ovl_act.py","prototypes/overlay-commit/report.py","prototypes/overlay-commit/suite.py","prototypes/overlay-commit/test_commit.py","prototypes/overlay-commit/verify_results.py"] | 0 | 0.045269 |  |
| docs-check.json | ["cargo","xtask","docs-check"] | 0 | 6.321734 | 文档链接检查通过：17 个 Markdown 文件<br>   Compiling proc-macro2 v1.0.107<br>   Compiling quote v1.0.47<br>   Compiling unicode-ident v1.0.27<br>   Compiling serde_core v1.0.229<br>   Compiling memchr v2.8.3<br>   Compiling utf8parse v0.2.2<br>   Compiling serde v1.0.229<br>   Compiling zmij v1.0.23<br>   Compiling anstyle-parse v1.0.0<br>   Compiling camino v1.2.6<br>   Compiling is_terminal_polyfill v1.70.2<br>   Compiling thiserror v2.0.21<br>   Compiling colorchoice v1.0.5<br>   Compiling serde_json v1.0.154<br>   Compiling anstyle v1.0.14<br>   Compiling anstyle-query v1.1.5<br>   Compiling itoa v1.0.19<br>   Compiling clap_lex v1.1.1<br>   Compiling anstream v1.0.0<br>   Compiling aho-corasick v1.1.5<br>   Compiling anyhow v1.0.104<br>   Compiling strsim v0.11.1<br>   Compiling regex-syntax v0.8.11<br>   Compiling heck v0.5.0<br>   Compiling syn v3.0.10<br>   Compiling clap_builder v4.6.7<br>   Compiling regex-automata v0.4.18<br>   Compiling semver v1.0.28<br>   Compiling serde_derive v1.0.229<br>   Compiling thiserror-impl v2.0.21<br>   Compiling clap_derive v4.6.7<br>   Compiling regex v1.13.1<br>   Compiling clap v4.6.7<br>   Compiling cargo-platform v0.1.9<br>   Compiling cargo_metadata v0.19.2<br>   Compiling xtask v0.0.0 (/home/gcp-vegas/.t3/worktrees/lancet/t3-dc7a648f/xtask)<br>    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.27s<br>     Running `target/debug/xtask docs-check`<br> |
| data-validation.json | ["python3","prototypes/overlay-commit/verify_results.py"] | 0 | 0.105723 | {"runs":84,"variants":28,"excluded":10,"errors":[],"outcomes":{"committed":39,"conflict":9,"failed-discarded":21,"failed-kept-regenerable":3,"discarded":12},"expectation_true":75,"confusion":{"C1":{"false_positive":5,"false_negative":0},"C4":{"false_positive":5,"false_negative":0}}} |

## 未覆盖范围

| 项 | 范围 |
| --- | --- |
| 文件系统 | 仅当前 XFS、当前内核与 SELinux；未测其他文件系统/内核 |
| 目标 | 真实仓库的单文件/三文件编辑与一个 crate；未做全 workspace build/test |
| 并发 | S9 预检-写回互撞和真实外部写者在提交过程中修改未覆盖 |
| C4 | 全部场景使用 both 并按 C1 提交；未单独按 C4 决策执行 |
| 记录时间 | 两段短于 24 小时；停止区间没有数据 |
| 工具范围 | S7 仅 mv、git mv、os.rename；redirect 仅分类测试，不跟随处理 |
| git stash | 提交后无新工作区改动；实际返回 No local changes to save；非空 stash 未覆盖 |

## 阻塞项（命令与原始报错）

| 记录 | 命令 | 连续失败次数 | 原始报错 |
| --- | --- | --- | --- |
| blocked-cleanup.json | ["python3","prototypes/overlay-commit/cleanup.py"] | 2 | PermissionError(13, 'Permission denied') |
| blocked-origin.json | ["bot-run","git","commit","-m","t"] | 3 | bot-run: origin/pushurl 不是有效的单一 GitHub 仓库地址，或使用了自定义 SSH Host 别名。<br> |

## 阻塞项处理记录

| 项 | 值 |
| --- | --- |
| status | 已修复后重跑 |
| repair_commands | [{"command":["git","-C","/home/gcp-vegas/exp/overlay/repo","remote","set-url","origin","https://github.com/Skyline10124/lattice.git"],"code":0,"stdout":"","stderr":"","seconds":0.0025861200410872698},{"command":["git","-C","/home/gcp-vegas/exp/overlay/baseline","remote","set-url","origin","https://github.com/Skyline10124/lattice.git"],"code":0,"stdout":"","stderr":"","seconds":0.002240270026959479}] |
| identity_check | {"command":["unshare","--user","--map-root-user","--mount","bot-run","git","var","GIT_AUTHOR_IDENT"],"code":0,"stdout":"lattice-devbot[bot] <325403158+lattice-devbot[bot]@users.noreply.github.com> 1794498106 -0800\n","stderr":"","seconds":0.9226976719801314} |
| excluded_runs | ["S6-quiet-commit-P1-1-1794498018335166402","S6-quiet-commit-P1-2-1794498024355147660","S6-quiet-commit-P1-3-1794498030326384829","S6-quiet-discard-P1-1-1794498036782811223","S6-quiet-discard-P1-2-1794498042840064819","S6-quiet-discard-P1-3-1794498048795038828","S6-quiet-commit-P2-1-1794498054717579339","S6-quiet-commit-P2-2-1794498060116325115","S6-quiet-commit-P2-3-1794498066487962252","S6-quiet-discard-P2-1-1794498072401595663"] |
| interrupted_runs | ["S6-quiet-discard-P2-1-1794498072401595663"] |
| driver_deviation | 首批驱动只将 namespace blocked 识别为基础设施失败，因此同变体第3次仍运行；现已将 bot-run 前置拒绝分类为 blocked |

## 清理阻塞处理

| 项 | 值 |
| --- | --- |
| status | 已解决；剩余 upper/work 0 |
| affected_directories | 94 |
| raw | cleanup-first-attempt.json |
| remaining | [] |

## 清理

| 项 | 原始值 |
| --- | --- |
| 删除 upper/work 目录数（两次合计） | 188 |
| 剩余 upper/work | [] |
| 清理前 / 后可用字节 | [79462170624,79462260736] |
| 活动实验进程 / overlay 挂载 | [[],[]] |
| 最终克隆 status |  |
| 最终克隆 fsck code | 0 |
| 保留路径 | ["/home/gcp-vegas/exp/overlay/repo","/home/gcp-vegas/exp/overlay/state/*/preimage","/home/gcp-vegas/exp/overlay/state/*/*.json","/home/gcp-vegas/exp/overlay/baseline","/home/gcp-vegas/exp/overlay/scripts","/home/gcp-vegas/.t3/worktrees/lancet/t3-dc7a648f/prototypes/overlay-commit/results"] |
| 清理失败 | [] |
| 记录器状态 | MainPID=2800672<br>ActiveState=active<br>SubState=running<br>ActiveEnterTimestamp=Thu 2026-11-12 07:01:14 PST<br> |
| 完整清理清单 | [cleanup.json](results/cleanup.json) |

## 原始文件封装

| 项 | 值 |
| --- | --- |
| 压缩前结果总字节 | 21036316 |
| 阈值字节 | 20971520 |
| gzip 运行数 | 94 |
| 清单与原文 SHA-256 | [packaging.json](results/packaging.json) |

## 场景与策略汇总

| 场景 | 策略 | 符合期望 / 完整运行 | C1 误报 / 漏报 | C4 误报 / 漏报 |
| --- | --- | --- | --- | --- |
| S1 | P1 | [3,3] | [0,0] | [0,0] |
| S2 | P1 | [3,3] | [0,0] | [0,0] |
| S2 | P2 | [3,3] | [0,0] | [0,0] |
| S2 | P3 | [3,3] | [0,0] | [0,0] |
| S3 | P1 | [3,3] | [0,0] | [0,0] |
| S4 | P1 | [3,3] | [0,0] | [0,0] |
| S5 | P1 | [6,6] | [0,0] | [0,0] |
| S6 | P1 | [9,12] | [5,0] | [5,0] |
| S6 | P2 | [12,12] | [0,0] | [0,0] |
| S6 | P3 | [6,12] | [0,0] | [0,0] |
| S7 | P1 | [9,9] | [0,0] | [0,0] |
| S8 | P1 | [3,3] | [0,0] | [0,0] |
| S9 | P1 | [12,12] | [0,0] | [0,0] |

## 冲突检查总数

| 检查 | 误报路径次数 | 漏报路径次数 |
| --- | --- | --- |
| C1 | 5 | 0 |
| C4 | 5 | 0 |
