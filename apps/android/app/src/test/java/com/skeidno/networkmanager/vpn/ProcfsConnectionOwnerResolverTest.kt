package com.skeidno.networkmanager.vpn

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ProcfsConnectionOwnerResolverTest {
    @Test
    fun resolvesBlueStacksIpv4MappedTcp6Socket() {
        val lines = sequenceOf(
            "sl local_address rem_address st tx_queue:rx_queue tr:tm->when retrnsmt uid timeout inode",
            "4: 0000000000000000FFFF0000010013AC:A69C " +
                "0000000000000000FFFF0000C90012C6:01BB 01 00000000:00000000 " +
                "00:00000000 00000000 10057 0 157687",
        )

        val uid = ProcfsConnectionOwnerResolver.findUid(lines, "172.19.0.1", 42652)

        assertEquals(10057, uid)
    }

    @Test
    fun ignoresSocketOwnedByAnotherSourceAddress() {
        val lines = sequenceOf(
            "sl local_address rem_address st tx_queue:rx_queue tr:tm->when retrnsmt uid timeout inode",
            "4: 0000000000000000FFFF0000010013AC:A69C " +
                "0000000000000000FFFF0000C90012C6:01BB 01 00000000:00000000 " +
                "00:00000000 00000000 10057 0 157687",
        )

        assertNull(ProcfsConnectionOwnerResolver.findUid(lines, "10.0.2.15", 42652))
    }
}
