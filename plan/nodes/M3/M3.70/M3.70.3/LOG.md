# Log M3.70.3

## 2026-11-23T14:06 [lead]

created (task) Image engine fidelity: inherit base-image ENV/CMD and resolve relative COPY against WORKDIR

## 2026-11-23T14:06 [lead]

status ready -> active

## 2026-11-23T14:06 [lead]

attempt 1/3 started

## 2026-11-23T14:06 [lead]

base a08bc288eeb341e92411f52837ec31916a26baea

## 2026-11-23T14:08 [lead]

fixed base ImageConfig seeding + Destination(workdir) resolution; 6 unit tests incl base PATH expansion and WORKDIR-relative COPY/--from; clippy+fmt green

## 2026-11-23T14:08 [lead]

status active -> done
