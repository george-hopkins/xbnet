/*! tun virtual IP gateway */

/*
    Copyright (C) 2019-2020 John Goerzen <jgoerzen@complete.org

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.

    You should have received a copy of the GNU General Public License
    along with this program.  If not, see <http://www.gnu.org/licenses/>.

*/

use tun_tap::{Iface, Mode};

use crate::ser::*;
use crate::xb::*;
use crate::xbpacket::*;
use crate::xbrx::*;
use bytes::*;
use crossbeam_channel;
use etherparse::*;
use log::*;
use std::collections::HashMap;
use std::convert::TryInto;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const XB_BROADCAST: u64 = 0xffff;

#[derive(Clone)]
pub struct XBTun {
    pub myxbmac: u64,
    pub name: String,
    pub broadcast_everything: bool,
    pub tun: Arc<Iface>,
    pub max_ip_cache: Duration,

    /** The map from IP Addresses (v4 or v6) to destination MAC addresses.  Also
    includes a timestamp at which the destination expires. */
    pub dests: Arc<Mutex<HashMap<IpAddr, (u64, Instant)>>>,
}

impl XBTun {
    pub fn new_tap(
        myxbmac: u64,
        broadcast_everything: bool,
        iface_name_requested: String,
        max_ip_cache: Duration,
    ) -> io::Result<XBTap> {
        let tun = Iface::without_packet_info(&iface_name_requested, Mode::Tun)?;
        let name = tun.name();

        println!("Interface {} (XBee MAC {:x}) ready", name, myxbmac,);

        let mut desthm = HashMap::new();

        Ok(XBTun {
            myxbmac,
            broadcast_everything,
            max_ip_cache,
            name: String::from(name),
            tun: Arc::new(tun),
            dests: Arc::new(Mutex::new(desthm)),
        })
    }

    pub fn get_xb_dest_mac(&self, ipaddr: &IpAddr) -> u64 {
        if self.broadcast_everything {
            return XB_BROADCAST;
        }

        match self.dests.lock().unwrap().get(ipaddr) {
            // Broadcast if we don't know it
            None => XB_BROADCAST,
            Some((dest, expiration)) => {
                if *expiration >= Instant::now() {
                    // Broadcast it if the cache entry has expired
                    XB_BROADCAST
                } else {
                    *dest
                }
            }
        }
    }

    pub fn frames_from_tun_processor(
        &self,
        maxframesize: usize,
        sender: crossbeam_channel::Sender<XBTX>,
    ) -> io::Result<()> {
        let mut buf = [0u8; 9100]; // Enough to handle even jumbo frames
        loop {
            let size = self.tun.recv(&mut buf)?;
            let tundata = &buf[0..size];
            trace!("TUNIN: {}", hex::encode(tundata));
            match SlicedPacket::from_ip(tundata) {
                Err(x) => {
                    warn!("Error parsing packet from tun; discarding: {:?}", x);
                }
                Ok(packet) => {
                    let destination = extract_ip(&packet);
                    if let Some(destination) = destination {
                        let destxbmac = self.get_xb_dest_mac(&destination);
                        trace!("TAPIN: Packet dest {} (MAC {:x})", destination, destxbmac);
                        let res = sender.try_send(XBTX::TXData(
                            XBDestAddr::U64(destxbmac),
                            Bytes::copy_from_slice(tundata),
                        ));
                        match res {
                            Ok(()) => (),
                            Err(crossbeam_channel::TrySendError::Full(_)) => {
                                debug!("Dropped packet due to full TX buffer")
                            }
                            Err(e) => Err(e).unwrap(),
                        }
                    } else {
                        warn!("Unable to get IP header from tun packet; discarding");
                    }
                }
            }
        }
    }

    pub fn frames_from_xb_processor(
        &self,
        xbreframer: &mut XBReframer,
        ser: &mut XBSerReader,
    ) -> io::Result<()> {
        loop {
            let (fromu64, _fromu16, payload) = xbreframer.rxframe(ser);

            // Register the sender in our map of known MACs
            match SlicedPacket::from_ip(&payload) {
                Err(x) => {
                    warn!(
                        "Packet from XBee wasn't valid IPv4 or IPv6; continueing anyhow: {:?}",
                        x
                    );
                }
                Ok(packet) => {
                    let toinsert = extract_ip(&packet);
                    if let Some(toinsert) = toinsert {
                        trace!("SERIN: Packet dest is -> {}", toinsert);
                        if !self.broadcast_everything {
                            self.dests.lock().unwrap().insert(
                                header.source().try_into().unwrap(),
                                (
                                    toinsert,
                                    Instant::now().checked_add(self.max_ip_cache).unwrap(),
                                ),
                            );
                        }
                    }
                }
            }

            self.tun.send(&payload)?;
        }
    }
}

pub fn showmac(mac: &[u8; 6]) -> String {
    format!(
        "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub fn extract_ip<'a>(packet: &SlicedPacket<'a>) -> Option<IpAddr> {
    match packet.ip {
        Some(InternetSlice::Ipv4(header)) => Some(IpAddr::V4(header.destination_addr())),
        Some(InternetSlice::Ipv6(header, _)) => Some(IpAddr::V6(header.destination_addr())),
        _ => None,
    }
}