public enum IOSSmokeCase: CaseIterable {
    case replicationRoundtrip
    case conflictAbort
    case invalidTableName
    case conflictHandlerPanicMapsError
}

@_silgen_name("diesel_sqlite_session_smoke_replication_roundtrip")
private func ffiReplicationRoundtrip() -> Int32

@_silgen_name("diesel_sqlite_session_smoke_conflict_abort")
private func ffiConflictAbort() -> Int32

@_silgen_name("diesel_sqlite_session_smoke_invalid_table_name")
private func ffiInvalidTableName() -> Int32

@_silgen_name("diesel_sqlite_session_smoke_conflict_handler_panic_maps_error")
private func ffiConflictHandlerPanicMapsError() -> Int32

public func runSmokeCase(_ smokeCase: IOSSmokeCase) -> Int32 {
    switch smokeCase {
    case .replicationRoundtrip:
        return ffiReplicationRoundtrip()
    case .conflictAbort:
        return ffiConflictAbort()
    case .invalidTableName:
        return ffiInvalidTableName()
    case .conflictHandlerPanicMapsError:
        return ffiConflictHandlerPanicMapsError()
    }
}
