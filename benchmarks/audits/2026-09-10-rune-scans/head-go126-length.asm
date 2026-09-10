TEXT github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB) /private/tmp/vibescript-rune-scans/internal/runtime/members_string.go
  members_string.go:1186	0x1004b4f90		f9400b90		MOVD 16(R28), R16
  members_string.go:1186	0x1004b4f94		eb3063ff		CMP R16, RSP
  members_string.go:1186	0x1004b4f98		540005a9		BLS 45(PC)
  members_string.go:1186	0x1004b4f9c		f81d0ffe		MOVD.W R30, -48(RSP)
  members_string.go:1186	0x1004b4fa0		f81f83fd		MOVD R29, -8(RSP)
  members_string.go:1186	0x1004b4fa4		d10023fd		SUB $8, RSP, R29
  ascii_scan_scalar.go:6	0x1004b4fa8		f90023e1		MOVD R1, 64(RSP)
  ascii_scan_scalar.go:6	0x1004b4fac		f9001fe0		MOVD R0, 56(RSP)
  ascii_scan_scalar.go:6	0x1004b4fb0		f100043f		CMP $1, R1
  ascii_scan_scalar.go:6	0x1004b4fb4		540000ed		BLE 7(PC)
  ascii_scan_scalar.go:6	0x1004b4fb8		39400003		MOVBU (R0), R3
  ascii_scan_scalar.go:6	0x1004b4fbc		39400404		MOVBU 1(R0), R4
  ascii_scan_scalar.go:6	0x1004b4fc0		aa040063		ORR R4, R3, R3
  ascii_scan_scalar.go:6	0x1004b4fc4		d3401c63		UBFX $0, R3, $8, R3
  ascii_scan_scalar.go:6	0x1004b4fc8		7102007f		CMPW $128, R3
  ascii_scan_scalar.go:6	0x1004b4fcc		540000a2		BCS 5(PC)
  ascii_scan_scalar.go:9	0x1004b4fd0		97f900a8		CALL github.com/mgomes/vibescript/internal/runtime.stringIsASCIIWords(SB)
  members_string.go:1187	0x1004b4fd4		370000c0		TBNZ $0, R0, 6(PC)
  utf8.go:448			0x1004b4fd8		f9401fe0		MOVD 56(RSP), R0
  utf8.go:448			0x1004b4fdc		f94023e1		MOVD 64(RSP), R1
  utf8.go:448			0x1004b4fe0		aa1f03e2		MOVD ZR, R2
  utf8.go:448			0x1004b4fe4		aa1f03e3		MOVD ZR, R3
  utf8.go:448			0x1004b4fe8		14000007		JMP 7(PC)
  members_string.go:1188	0x1004b4fec		f94023e0		MOVD 64(RSP), R0
  members_string.go:1188	0x1004b4ff0		f85f83fd		MOVD -8(RSP), R29
  members_string.go:1188	0x1004b4ff4		f84307fe		MOVD.P 48(RSP), R30
  members_string.go:1188	0x1004b4ff8		d65f03c0		RET
  utf8.go:449			0x1004b4ffc		91000463		ADD $1, R3, R3
  utf8.go:448			0x1004b5000		aa0403e2		MOVD R4, R2
  utf8.go:448			0x1004b5004		eb02003f		CMP R2, R1
  utf8.go:448			0x1004b5008		540001ad		BLE 13(PC)
  utf8.go:448			0x1004b500c		38626804		MOVBU (R0)(R2), R4
  utf8.go:448			0x1004b5010		7101fc9f		CMPW $127, R4
  utf8.go:448			0x1004b5014		5400006c		BGT 3(PC)
  utf8.go:448			0x1004b5018		91000444		ADD $1, R2, R4
  utf8.go:448			0x1004b501c		17fffff8		JMP -8(PC)
  utf8.go:448			0x1004b5020		f90013e3		MOVD R3, 32(RSP)
  utf8.go:448			0x1004b5024		97ef1e6b		CALL runtime.decoderune(SB)
  utf8.go:448			0x1004b5028		f9401fe0		MOVD 56(RSP), R0
  utf8.go:449			0x1004b502c		f94013e3		MOVD 32(RSP), R3
  utf8.go:449			0x1004b5030		aa0103e4		MOVD R1, R4
  utf8.go:448			0x1004b5034		f94023e1		MOVD 64(RSP), R1
  utf8.go:448			0x1004b5038		17fffff1		JMP -15(PC)
  members_string.go:1190	0x1004b503c		aa0303e0		MOVD R3, R0
  members_string.go:1190	0x1004b5040		f85f83fd		MOVD -8(RSP), R29
  members_string.go:1190	0x1004b5044		f84307fe		MOVD.P 48(RSP), R30
  members_string.go:1190	0x1004b5048		d65f03c0		RET
  members_string.go:1186	0x1004b504c		a90087e0		STP (R0, R1), 8(RSP)
  members_string.go:1186	0x1004b5050		aa1e03e3		MOVD R30, R3
  members_string.go:1186	0x1004b5054		97ef4d9b		CALL runtime.morestack_noctxt.abi0(SB)
  members_string.go:1186	0x1004b5058		a94087e0		LDP 8(RSP), (R0, R1)
  members_string.go:1186	0x1004b505c		17ffffcd		JMP github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB)

TEXT github.com/mgomes/vibescript/internal/runtime.stringMemberQuery.func2(SB) /private/tmp/vibescript-rune-scans/internal/runtime/members_string.go
  members_string.go:3820	0x1008245d0		f9400b90		MOVD 16(R28), R16
  members_string.go:3820	0x1008245d4		eb3063ff		CMP R16, RSP
  members_string.go:3820	0x1008245d8		54000669		BLS 51(PC)
  members_string.go:3820	0x1008245dc		f81c0ffe		MOVD.W R30, -64(RSP)
  members_string.go:3820	0x1008245e0		f81f83fd		MOVD R29, -8(RSP)
  members_string.go:3820	0x1008245e4		d10023fd		SUB $8, RSP, R29
  members_string.go:3820	0x1008245e8		f9002fe2		MOVD R2, 88(RSP)
  members_string.go:3820	0x1008245ec		f90033e3		MOVD R3, 96(RSP)
  members_string.go:3820	0x1008245f0		f9003be5		MOVD R5, 112(RSP)
  members_string.go:3821	0x1008245f4		b40003a6		CBZ R6, 29(PC)
  errors.go:26			0x1008245f8		900005e0		ADRP 770048(PC), R0
  errors.go:26			0x1008245fc		91281400		ADD $2565, R0, R0
  errors.go:26			0x100824600		d28004a1		MOVD $37, R1
  errors.go:26			0x100824604		aa1f03e2		MOVD ZR, R2
  errors.go:26			0x100824608		aa1f03e3		MOVD ZR, R3
  errors.go:26			0x10082460c		aa1f03e4		MOVD ZR, R4
  errors.go:26			0x100824610		97e37328		CALL fmt.errorf(SB)
  errors.go:26			0x100824614		b5000180		CBNZ R0, 12(PC)
  errors.go:31			0x100824618		d503201f		NOOP
  errors.go:65			0x10082461c		b0004c80		ADRP 10031104(PC), R0
  errors.go:65			0x100824620		912f0000		ADD $3008, R0, R0
  errors.go:65			0x100824624		97dfe99b		CALL runtime.newobject(SB)
  errors.go:65			0x100824628		900005e1		ADRP 770048(PC), R1
  errors.go:65			0x10082462c		91281421		ADD $2565, R1, R1
  errors.go:65			0x100824630		d28004a2		MOVD $37, R2
  errors.go:65			0x100824634		a9000801		STP (R1, R2), (R0)
  members_string.go:3822	0x100824638		aa0003e1		MOVD R0, R1
  members_string.go:3822	0x10082463c		90005220		ADRP 10764288(PC), R0
  members_string.go:3822	0x100824640		9101e000		ADD $120, R0, R0
  members_string.go:3822	0x100824644		aa1f03e2		MOVD ZR, R2
  members_string.go:3822	0x100824648		aa1f03e3		MOVD ZR, R3
  members_string.go:3822	0x10082464c		aa0003e4		MOVD R0, R4
  members_string.go:3822	0x100824650		aa0103e5		MOVD R1, R5
  members_string.go:3822	0x100824654		aa1f03e0		MOVD ZR, R0
  members_string.go:3822	0x100824658		aa1f03e1		MOVD ZR, R1
  members_string.go:3822	0x10082465c		f85f83fd		MOVD -8(RSP), R29
  members_string.go:3822	0x100824660		f84407fe		MOVD.P 64(RSP), R30
  members_string.go:3822	0x100824664		d65f03c0		RET
  members_string.go:3824	0x100824668		aa0103e0		MOVD R1, R0
  members_string.go:3824	0x10082466c		aa0203e1		MOVD R2, R1
  members_string.go:3824	0x100824670		aa0303e2		MOVD R3, R2
  members_string.go:3824	0x100824674		aa0403e3		MOVD R4, R3
  members_string.go:3824	0x100824678		97e77cb6		CALL github.com/mgomes/vibescript/vibes/value.Value.String(SB)
  members_string.go:3824	0x10082467c		97f24245		CALL github.com/mgomes/vibescript/internal/runtime.stringRuneLen(SB)
  members_string.go:3824	0x100824680		aa1f03e1		MOVD ZR, R1
  members_string.go:3824	0x100824684		aa1f03e2		MOVD ZR, R2
  members_string.go:3824	0x100824688		aa0003e3		MOVD R0, R3
  members_string.go:3824	0x10082468c		aa1f03e4		MOVD ZR, R4
  members_string.go:3824	0x100824690		aa1f03e5		MOVD ZR, R5
  members_string.go:3824	0x100824694		b27f03e0		ORR $2, ZR, R0
  members_string.go:3824	0x100824698		f85f83fd		MOVD -8(RSP), R29
  members_string.go:3824	0x10082469c		f84407fe		MOVD.P 64(RSP), R30
  members_string.go:3824	0x1008246a0		d65f03c0		RET
  members_string.go:3820	0x1008246a4		a90087e0		STP (R0, R1), 8(RSP)
  members_string.go:3820	0x1008246a8		a9018fe2		STP (R2, R3), 24(RSP)
  members_string.go:3820	0x1008246ac		a90297e4		STP (R4, R5), 40(RSP)
  members_string.go:3820	0x1008246b0		a9039fe6		STP (R6, R7), 56(RSP)
  members_string.go:3820	0x1008246b4		a904a7e8		STP (R8, R9), 72(RSP)
  members_string.go:3820	0x1008246b8		a905afea		STP (R10, R11), 88(RSP)
  members_string.go:3820	0x1008246bc		f90037ec		MOVD R12, 104(RSP)
  members_string.go:3820	0x1008246c0		aa1e03e3		MOVD R30, R3
  members_string.go:3820	0x1008246c4		97e18fff		CALL runtime.morestack_noctxt.abi0(SB)
  members_string.go:3820	0x1008246c8		a94087e0		LDP 8(RSP), (R0, R1)
  members_string.go:3820	0x1008246cc		a9418fe2		LDP 24(RSP), (R2, R3)
  members_string.go:3820	0x1008246d0		a94297e4		LDP 40(RSP), (R4, R5)
  members_string.go:3820	0x1008246d4		a9439fe6		LDP 56(RSP), (R6, R7)
  members_string.go:3820	0x1008246d8		a944a7e8		LDP 72(RSP), (R8, R9)
  members_string.go:3820	0x1008246dc		a945afea		LDP 88(RSP), (R10, R11)
  members_string.go:3820	0x1008246e0		f94037ec		MOVD 104(RSP), R12
  members_string.go:3820	0x1008246e4		17ffffbb		JMP github.com/mgomes/vibescript/internal/runtime.stringMemberQuery.func2(SB)
  members_string.go:3820	0x1008246e8		00000000		?
  members_string.go:3820	0x1008246ec		00000000		?
