TEXT github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB) /private/tmp/vibescript-rune-before/internal/runtime/members_string.go
  members_string.go:1186	0x1004b4fa0		f9400b90		MOVD 16(R28), R16
  members_string.go:1186	0x1004b4fa4		eb3063ff		CMP R16, RSP
  members_string.go:1186	0x1004b4fa8		540005a9		BLS 45(PC)
  members_string.go:1186	0x1004b4fac		f81d0ffe		MOVD.W R30, -48(RSP)
  members_string.go:1186	0x1004b4fb0		f81f83fd		MOVD R29, -8(RSP)
  members_string.go:1186	0x1004b4fb4		d10023fd		SUB $8, RSP, R29
  ascii_scan_scalar.go:6	0x1004b4fb8		f90023e1		MOVD R1, 64(RSP)
  ascii_scan_scalar.go:6	0x1004b4fbc		f9001fe0		MOVD R0, 56(RSP)
  ascii_scan_scalar.go:6	0x1004b4fc0		f100043f		CMP $1, R1
  ascii_scan_scalar.go:6	0x1004b4fc4		540000ed		BLE 7(PC)
  ascii_scan_scalar.go:6	0x1004b4fc8		39400003		MOVBU (R0), R3
  ascii_scan_scalar.go:6	0x1004b4fcc		39400404		MOVBU 1(R0), R4
  ascii_scan_scalar.go:6	0x1004b4fd0		aa040063		ORR R4, R3, R3
  ascii_scan_scalar.go:6	0x1004b4fd4		d3401c63		UBFX $0, R3, $8, R3
  ascii_scan_scalar.go:6	0x1004b4fd8		7102007f		CMPW $128, R3
  ascii_scan_scalar.go:6	0x1004b4fdc		540000a2		BCS 5(PC)
  ascii_scan_scalar.go:9	0x1004b4fe0		97f900a4		CALL github.com/mgomes/vibescript/internal/runtime.stringIsASCIIWords(SB)
  members_string.go:1187	0x1004b4fe4		370000c0		TBNZ $0, R0, 6(PC)
  utf8.go:448			0x1004b4fe8		f9401fe0		MOVD 56(RSP), R0
  utf8.go:448			0x1004b4fec		f94023e1		MOVD 64(RSP), R1
  utf8.go:448			0x1004b4ff0		aa1f03e2		MOVD ZR, R2
  utf8.go:448			0x1004b4ff4		aa1f03e3		MOVD ZR, R3
  utf8.go:448			0x1004b4ff8		14000007		JMP 7(PC)
  members_string.go:1188	0x1004b4ffc		f94023e0		MOVD 64(RSP), R0
  members_string.go:1188	0x1004b5000		f85f83fd		MOVD -8(RSP), R29
  members_string.go:1188	0x1004b5004		f84307fe		MOVD.P 48(RSP), R30
  members_string.go:1188	0x1004b5008		d65f03c0		RET
  utf8.go:449			0x1004b500c		91000463		ADD $1, R3, R3
  utf8.go:448			0x1004b5010		aa0403e2		MOVD R4, R2
  utf8.go:448			0x1004b5014		eb02003f		CMP R2, R1
  utf8.go:448			0x1004b5018		540001ad		BLE 13(PC)
  utf8.go:448			0x1004b501c		38626804		MOVBU (R0)(R2), R4
  utf8.go:448			0x1004b5020		7101fc9f		CMPW $127, R4
  utf8.go:448			0x1004b5024		5400006c		BGT 3(PC)
  utf8.go:448			0x1004b5028		91000444		ADD $1, R2, R4
  utf8.go:448			0x1004b502c		17fffff8		JMP -8(PC)
  utf8.go:448			0x1004b5030		f90013e3		MOVD R3, 32(RSP)
  utf8.go:448			0x1004b5034		97ef1e67		CALL runtime.decoderune(SB)
  utf8.go:448			0x1004b5038		f9401fe0		MOVD 56(RSP), R0
  utf8.go:449			0x1004b503c		f94013e3		MOVD 32(RSP), R3
  utf8.go:449			0x1004b5040		aa0103e4		MOVD R1, R4
  utf8.go:448			0x1004b5044		f94023e1		MOVD 64(RSP), R1
  utf8.go:448			0x1004b5048		17fffff1		JMP -15(PC)
  members_string.go:1190	0x1004b504c		aa0303e0		MOVD R3, R0
  members_string.go:1190	0x1004b5050		f85f83fd		MOVD -8(RSP), R29
  members_string.go:1190	0x1004b5054		f84307fe		MOVD.P 48(RSP), R30
  members_string.go:1190	0x1004b5058		d65f03c0		RET
  members_string.go:1186	0x1004b505c		a90087e0		STP (R0, R1), 8(RSP)
  members_string.go:1186	0x1004b5060		aa1e03e3		MOVD R30, R3
  members_string.go:1186	0x1004b5064		97ef4d97		CALL runtime.morestack_noctxt.abi0(SB)
  members_string.go:1186	0x1004b5068		a94087e0		LDP 8(RSP), (R0, R1)
  members_string.go:1186	0x1004b506c		17ffffcd		JMP github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB)

TEXT github.com/mgomes/vibescript/internal/runtime.stringMemberQuery.func2(SB) /private/tmp/vibescript-rune-before/internal/runtime/members_string.go
  members_string.go:3813	0x100823810		f9400b90		MOVD 16(R28), R16
  members_string.go:3813	0x100823814		eb3063ff		CMP R16, RSP
  members_string.go:3813	0x100823818		54000669		BLS 51(PC)
  members_string.go:3813	0x10082381c		f81c0ffe		MOVD.W R30, -64(RSP)
  members_string.go:3813	0x100823820		f81f83fd		MOVD R29, -8(RSP)
  members_string.go:3813	0x100823824		d10023fd		SUB $8, RSP, R29
  members_string.go:3813	0x100823828		f9002fe2		MOVD R2, 88(RSP)
  members_string.go:3813	0x10082382c		f90033e3		MOVD R3, 96(RSP)
  members_string.go:3813	0x100823830		f9003be5		MOVD R5, 112(RSP)
  members_string.go:3814	0x100823834		b40003a6		CBZ R6, 29(PC)
  errors.go:26			0x100823838		900005e0		ADRP 770048(PC), R0
  errors.go:26			0x10082383c		912e7000		ADD $2972, R0, R0
  errors.go:26			0x100823840		d28004a1		MOVD $37, R1
  errors.go:26			0x100823844		aa1f03e2		MOVD ZR, R2
  errors.go:26			0x100823848		aa1f03e3		MOVD ZR, R3
  errors.go:26			0x10082384c		aa1f03e4		MOVD ZR, R4
  errors.go:26			0x100823850		97e37698		CALL fmt.errorf(SB)
  errors.go:26			0x100823854		b5000180		CBNZ R0, 12(PC)
  errors.go:31			0x100823858		d503201f		NOOP
  errors.go:65			0x10082385c		d0004c60		ADRP 10018816(PC), R0
  errors.go:65			0x100823860		912d0000		ADD $2880, R0, R0
  errors.go:65			0x100823864		97dfed0b		CALL runtime.newobject(SB)
  errors.go:65			0x100823868		900005e1		ADRP 770048(PC), R1
  errors.go:65			0x10082386c		912e7021		ADD $2972, R1, R1
  errors.go:65			0x100823870		d28004a2		MOVD $37, R2
  errors.go:65			0x100823874		a9000801		STP (R1, R2), (R0)
  members_string.go:3815	0x100823878		aa0003e1		MOVD R0, R1
  members_string.go:3815	0x10082387c		90005200		ADRP 10747904(PC), R0
  members_string.go:3815	0x100823880		913f6000		ADD $4056, R0, R0
  members_string.go:3815	0x100823884		aa1f03e2		MOVD ZR, R2
  members_string.go:3815	0x100823888		aa1f03e3		MOVD ZR, R3
  members_string.go:3815	0x10082388c		aa0003e4		MOVD R0, R4
  members_string.go:3815	0x100823890		aa0103e5		MOVD R1, R5
  members_string.go:3815	0x100823894		aa1f03e0		MOVD ZR, R0
  members_string.go:3815	0x100823898		aa1f03e1		MOVD ZR, R1
  members_string.go:3815	0x10082389c		f85f83fd		MOVD -8(RSP), R29
  members_string.go:3815	0x1008238a0		f84407fe		MOVD.P 64(RSP), R30
  members_string.go:3815	0x1008238a4		d65f03c0		RET
  members_string.go:3817	0x1008238a8		aa0103e0		MOVD R1, R0
  members_string.go:3817	0x1008238ac		aa0203e1		MOVD R2, R1
  members_string.go:3817	0x1008238b0		aa0303e2		MOVD R3, R2
  members_string.go:3817	0x1008238b4		aa0403e3		MOVD R4, R3
  members_string.go:3817	0x1008238b8		97e78026		CALL github.com/mgomes/vibescript/vibes/value.Value.String(SB)
  members_string.go:3817	0x1008238bc		97f245b9		CALL github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB)
  members_string.go:3817	0x1008238c0		aa1f03e1		MOVD ZR, R1
  members_string.go:3817	0x1008238c4		aa1f03e2		MOVD ZR, R2
  members_string.go:3817	0x1008238c8		aa0003e3		MOVD R0, R3
  members_string.go:3817	0x1008238cc		aa1f03e4		MOVD ZR, R4
  members_string.go:3817	0x1008238d0		aa1f03e5		MOVD ZR, R5
  members_string.go:3817	0x1008238d4		b27f03e0		ORR $2, ZR, R0
  members_string.go:3817	0x1008238d8		f85f83fd		MOVD -8(RSP), R29
  members_string.go:3817	0x1008238dc		f84407fe		MOVD.P 64(RSP), R30
  members_string.go:3817	0x1008238e0		d65f03c0		RET
  members_string.go:3813	0x1008238e4		a90087e0		STP (R0, R1), 8(RSP)
  members_string.go:3813	0x1008238e8		a9018fe2		STP (R2, R3), 24(RSP)
  members_string.go:3813	0x1008238ec		a90297e4		STP (R4, R5), 40(RSP)
  members_string.go:3813	0x1008238f0		a9039fe6		STP (R6, R7), 56(RSP)
  members_string.go:3813	0x1008238f4		a904a7e8		STP (R8, R9), 72(RSP)
  members_string.go:3813	0x1008238f8		a905afea		STP (R10, R11), 88(RSP)
  members_string.go:3813	0x1008238fc		f90037ec		MOVD R12, 104(RSP)
  members_string.go:3813	0x100823900		aa1e03e3		MOVD R30, R3
  members_string.go:3813	0x100823904		97e1936f		CALL runtime.morestack_noctxt.abi0(SB)
  members_string.go:3813	0x100823908		a94087e0		LDP 8(RSP), (R0, R1)
  members_string.go:3813	0x10082390c		a9418fe2		LDP 24(RSP), (R2, R3)
  members_string.go:3813	0x100823910		a94297e4		LDP 40(RSP), (R4, R5)
  members_string.go:3813	0x100823914		a9439fe6		LDP 56(RSP), (R6, R7)
  members_string.go:3813	0x100823918		a944a7e8		LDP 72(RSP), (R8, R9)
  members_string.go:3813	0x10082391c		a945afea		LDP 88(RSP), (R10, R11)
  members_string.go:3813	0x100823920		f94037ec		MOVD 104(RSP), R12
  members_string.go:3813	0x100823924		17ffffbb		JMP github.com/mgomes/vibescript/internal/runtime.stringMemberQuery.func2(SB)
  members_string.go:3813	0x100823928		00000000		?
  members_string.go:3813	0x10082392c		00000000		?
