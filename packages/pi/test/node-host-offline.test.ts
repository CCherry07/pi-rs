import assert from 'node:assert/strict'
import test from 'node:test'

import { PiEvalNodeHost, PiNodeHost } from '../src/index.js'
import type { NativeBinding } from '../src/native-binding.js'

interface EnvironmentSnapshot {
  offline: string | undefined
  skipVersionCheck: string | undefined
}

function snapshotEnvironment(): EnvironmentSnapshot {
  return {
    offline: process.env.PI_OFFLINE,
    skipVersionCheck: process.env.PI_SKIP_VERSION_CHECK,
  }
}

function restoreEnvironment(snapshot: EnvironmentSnapshot): void {
  if (snapshot.offline === undefined) delete process.env.PI_OFFLINE
  else process.env.PI_OFFLINE = snapshot.offline
  if (snapshot.skipVersionCheck === undefined) delete process.env.PI_SKIP_VERSION_CHECK
  else process.env.PI_SKIP_VERSION_CHECK = snapshot.skipVersionCheck
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void
  const promise = new Promise<void>(done => {
    resolve = done
  })
  return { promise, resolve }
}

function binding(options: {
  runPi?: NativeBinding['runPi']
  runPiEval?: NativeBinding['runPiEval']
}): NativeBinding {
  return {
    runPi: options.runPi ?? (async () => 0),
    runPiEval: options.runPiEval ?? (async () => {}),
  }
}

test('normalizes offline variables for command and eval hosts then restores their prior values', async () => {
  const environment = snapshotEnvironment()
  process.env.PI_OFFLINE = 'original-offline'
  process.env.PI_SKIP_VERSION_CHECK = 'original-skip'
  try {
    const command = new PiNodeHost({
      arguments: ['--offline'],
      nativeBinding: binding({
        runPi: async () => {
          assert.equal(process.env.PI_OFFLINE, '1')
          assert.equal(process.env.PI_SKIP_VERSION_CHECK, '1')
          return 7
        },
      }),
    })
    assert.equal(await command.runWithExitCode(), 7)
    assert.equal(process.env.PI_OFFLINE, 'original-offline')
    assert.equal(process.env.PI_SKIP_VERSION_CHECK, 'original-skip')

    process.env.PI_OFFLINE = 'yes'
    const evaluation = new PiEvalNodeHost({
      nativeBinding: binding({
        runPiEval: async () => {
          assert.equal(process.env.PI_OFFLINE, '1')
          assert.equal(process.env.PI_SKIP_VERSION_CHECK, '1')
        },
      }),
    })
    await evaluation.run()
    assert.equal(process.env.PI_OFFLINE, 'yes')
    assert.equal(process.env.PI_SKIP_VERSION_CHECK, 'original-skip')
  } finally {
    restoreEnvironment(environment)
  }
})

test('restores offline variables and releases the process guard when the native host fails', async () => {
  const environment = snapshotEnvironment()
  delete process.env.PI_OFFLINE
  delete process.env.PI_SKIP_VERSION_CHECK
  try {
    const host = new PiNodeHost({
      arguments: ['--offline'],
      nativeBinding: binding({
        runPi: async () => {
          assert.equal(process.env.PI_OFFLINE, '1')
          assert.equal(process.env.PI_SKIP_VERSION_CHECK, '1')
          throw new Error('native failure')
        },
      }),
    })
    await assert.rejects(host.runWithExitCode(), /native failure/)
    assert.equal(process.env.PI_OFFLINE, undefined)
    assert.equal(process.env.PI_SKIP_VERSION_CHECK, undefined)

    const next = new PiNodeHost({ nativeBinding: binding({}) })
    assert.equal(await next.runWithExitCode(), 0)
  } finally {
    restoreEnvironment(environment)
  }
})

test('rejects overlap in either direction instead of leaking offline state between hosts', async () => {
  const environment = snapshotEnvironment()
  delete process.env.PI_OFFLINE
  delete process.env.PI_SKIP_VERSION_CHECK
  try {
    const releaseOffline = deferred()
    const offlineStarted = deferred()
    const offline = new PiNodeHost({
      arguments: ['--offline'],
      nativeBinding: binding({
        runPi: async () => {
          assert.equal(process.env.PI_OFFLINE, '1')
          offlineStarted.resolve()
          await releaseOffline.promise
          return 0
        },
      }),
    })
    const offlineRun = offline.runWithExitCode()
    await offlineStarted.promise

    let onlineCalled = false
    const onlineDuringOffline = new PiNodeHost({
      nativeBinding: binding({
        runPi: async () => {
          onlineCalled = true
          return 0
        },
      }),
    })
    await assert.rejects(
      onlineDuringOffline.runWithExitCode(),
      /PI_OFFLINE and PI_SKIP_VERSION_CHECK are process-wide/,
    )
    assert.equal(onlineCalled, false)
    assert.equal(process.env.PI_OFFLINE, '1')
    releaseOffline.resolve()
    await offlineRun
    assert.equal(process.env.PI_OFFLINE, undefined)
    assert.equal(process.env.PI_SKIP_VERSION_CHECK, undefined)

    const releaseOnline = deferred()
    const onlineStarted = deferred()
    const online = new PiNodeHost({
      nativeBinding: binding({
        runPi: async () => {
          assert.equal(process.env.PI_OFFLINE, undefined)
          onlineStarted.resolve()
          await releaseOnline.promise
          return 0
        },
      }),
    })
    const onlineRun = online.runWithExitCode()
    await onlineStarted.promise

    let secondOfflineCalled = false
    const offlineDuringOnline = new PiNodeHost({
      arguments: ['--offline'],
      nativeBinding: binding({
        runPi: async () => {
          secondOfflineCalled = true
          return 0
        },
      }),
    })
    await assert.rejects(
      offlineDuringOnline.runWithExitCode(),
      /PI_OFFLINE and PI_SKIP_VERSION_CHECK are process-wide/,
    )
    assert.equal(secondOfflineCalled, false)
    assert.equal(process.env.PI_OFFLINE, undefined)
    assert.equal(process.env.PI_SKIP_VERSION_CHECK, undefined)
    releaseOnline.resolve()
    await onlineRun
  } finally {
    restoreEnvironment(environment)
  }
})
