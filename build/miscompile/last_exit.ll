; `caller` returns `true`: `%iter` is -1 on the loop's thousandth pass, the one
; after `%nextnext` wrapped, which nothing reads. A loop of few enough passes
; to be evaluated is, before `indvars` meets it.
define i1 @caller() {
entry:
  br label %header

header:
  %next = phi i16 [ -999, %entry ], [ %nextnext, %latch ]
  %iter = phi i16 [ -1000, %entry ], [ %next, %latch ]
  %done = icmp eq i16 %iter, -1
  br i1 %done, label %exit, label %latch

latch:
  %nextnext = add nuw i16 %next, 1
  br label %header

exit:
  ret i1 true
}
