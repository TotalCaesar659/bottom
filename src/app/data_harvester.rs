//! This is the main file to house data collection functions.

use std::time::Instant;

#[cfg(target_os = "linux")]
use std::collections::HashMap;

use sysinfo::{System, SystemExt};

use battery::{Battery, Manager};

use crate::app::layout_manager::UsedWidgets;

use futures::join;

pub mod battery_harvester;
pub mod cpu;
pub mod disks;
pub mod mem;
pub mod network;
pub mod processes;
pub mod temperature;

#[derive(Clone, Debug)]
pub struct Data {
    pub last_collection_time: Instant,
    pub cpu: Option<cpu::CpuHarvest>,
    pub memory: Option<mem::MemHarvest>,
    pub swap: Option<mem::MemHarvest>,
    pub temperature_sensors: Option<Vec<temperature::TempHarvest>>,
    pub network: Option<network::NetworkHarvest>,
    pub list_of_processes: Option<Vec<processes::ProcessHarvest>>,
    pub disks: Option<Vec<disks::DiskHarvest>>,
    pub io: Option<disks::IOHarvest>,
    pub list_of_batteries: Option<Vec<battery_harvester::BatteryHarvest>>,
}

impl Default for Data {
    fn default() -> Self {
        Data {
            last_collection_time: Instant::now(),
            cpu: None,
            memory: None,
            swap: None,
            temperature_sensors: None,
            list_of_processes: None,
            disks: None,
            io: None,
            network: None,
            list_of_batteries: None,
        }
    }
}

impl Data {
    pub fn cleanup(&mut self) {
        self.io = None;
        self.temperature_sensors = None;
        self.list_of_processes = None;
        self.disks = None;
        self.memory = None;
        self.swap = None;
        self.cpu = None;

        if let Some(network) = &mut self.network {
            network.first_run_cleanup();
        }
    }
}

#[derive(Debug)]
pub struct DataCollector {
    pub data: Data,
    sys: System,
    #[cfg(target_os = "linux")]
    pid_mapping: HashMap<crate::Pid, processes::PrevProcDetails>,
    #[cfg(target_os = "linux")]
    prev_idle: f64,
    #[cfg(target_os = "linux")]
    prev_non_idle: f64,
    mem_total_kb: u64,
    temperature_type: temperature::TemperatureType,
    use_current_cpu_total: bool,
    last_collection_time: Instant,
    total_rx: u64,
    total_tx: u64,
    show_average_cpu: bool,
    widgets_to_harvest: UsedWidgets,
    battery_manager: Option<Manager>,
    battery_list: Option<Vec<Battery>>,
    #[cfg(target_os = "linux")]
    page_file_size_kb: u64,
}

impl Default for DataCollector {
    fn default() -> Self {
        trace!("Creating default data collector...");
        DataCollector {
            data: Data::default(),
            sys: System::new_all(),
            #[cfg(target_os = "linux")]
            pid_mapping: HashMap::new(),
            #[cfg(target_os = "linux")]
            prev_idle: 0_f64,
            #[cfg(target_os = "linux")]
            prev_non_idle: 0_f64,
            mem_total_kb: 0,
            temperature_type: temperature::TemperatureType::Celsius,
            use_current_cpu_total: false,
            last_collection_time: Instant::now(),
            total_rx: 0,
            total_tx: 0,
            show_average_cpu: false,
            widgets_to_harvest: UsedWidgets::default(),
            battery_manager: None,
            battery_list: None,
            #[cfg(target_os = "linux")]
            page_file_size_kb: unsafe {
                let page_file_size_kb = libc::sysconf(libc::_SC_PAGESIZE) as u64 / 1024;
                trace!("Page file size in KB: {}", page_file_size_kb);
                page_file_size_kb
            },
        }
    }
}

impl DataCollector {
    pub fn init(&mut self) {
        trace!("Initializing data collector.");
        self.mem_total_kb = self.sys.get_total_memory();
        trace!("Total memory in KB: {}", self.mem_total_kb);

        if self.widgets_to_harvest.use_battery {
            trace!("First run battery vec creation.");
            if let Ok(battery_manager) = Manager::new() {
                if let Ok(batteries) = battery_manager.batteries() {
                    let battery_list: Vec<Battery> = batteries.filter_map(Result::ok).collect();
                    if !battery_list.is_empty() {
                        self.battery_list = Some(battery_list);
                        self.battery_manager = Some(battery_manager);
                    }
                }
            }
        }

        trace!("Running first run.");
        futures::executor::block_on(self.update_data());
        trace!("First run done.  Sleeping for 250ms...");
        std::thread::sleep(std::time::Duration::from_millis(250));

        trace!("First run done.  Running first run cleanup now.");
        self.data.cleanup();

        trace!("Enabled widgets to harvest: {:#?}", self.widgets_to_harvest);
    }

    pub fn set_collected_data(&mut self, used_widgets: UsedWidgets) {
        self.widgets_to_harvest = used_widgets;
    }

    pub fn set_temperature_type(&mut self, temperature_type: temperature::TemperatureType) {
        self.temperature_type = temperature_type;
    }

    pub fn set_use_current_cpu_total(&mut self, use_current_cpu_total: bool) {
        self.use_current_cpu_total = use_current_cpu_total;
    }

    pub fn set_show_average_cpu(&mut self, show_average_cpu: bool) {
        self.show_average_cpu = show_average_cpu;
    }

    pub async fn update_data(&mut self) {
        if self.widgets_to_harvest.use_cpu {
            self.sys.refresh_cpu();
        }

        if cfg!(any(target_arch = "arm", target_arch = "aarch64")) {
            // ARM stuff
            if self.widgets_to_harvest.use_proc {
                self.sys.refresh_processes();
            }
            if self.widgets_to_harvest.use_temp {
                self.sys.refresh_components();
            }
            if self.widgets_to_harvest.use_net {
                self.sys.refresh_networks();
            }
            if self.widgets_to_harvest.use_mem {
                self.sys.refresh_memory();
            }
        } else {
            if cfg!(not(target_os = "linux")) {
                if self.widgets_to_harvest.use_proc {
                    self.sys.refresh_processes();
                }
                if self.widgets_to_harvest.use_temp {
                    self.sys.refresh_components();
                }
            }
            if cfg!(target_os = "windows") && self.widgets_to_harvest.use_net {
                self.sys.refresh_networks();
            }
        }

        let current_instant = std::time::Instant::now();

        // CPU
        if self.widgets_to_harvest.use_cpu {
            self.data.cpu = Some(cpu::get_cpu_data_list(&self.sys, self.show_average_cpu));
            if log_enabled!(log::Level::Trace) {
                if let Some(cpus) = &self.data.cpu {
                    trace!("cpus: {:#?} results", cpus.len());
                } else {
                    trace!("Found no cpus.");
                }
            }
        }

        // Batteries
        if let Some(battery_manager) = &self.battery_manager {
            if let Some(battery_list) = &mut self.battery_list {
                self.data.list_of_batteries = Some(battery_harvester::refresh_batteries(
                    &battery_manager,
                    battery_list,
                ));
            }

            if log_enabled!(log::Level::Trace) {
                if let Some(batteries) = &self.data.list_of_batteries {
                    trace!("batteries: {:#?} results", batteries.len());
                } else {
                    trace!("Found no batteries.");
                }
            }
        }

        if self.widgets_to_harvest.use_proc {
            // Processes.  This is the longest part of the harvesting process... changing this might be
            // good in the future.  What was tried already:
            // * Splitting the internal part into multiple scoped threads (dropped by ~.01 seconds, but upped usage)
            if let Ok(process_list) = if cfg!(target_os = "linux") {
                #[cfg(target_os = "linux")]
                {
                    processes::linux_processes(
                        &mut self.prev_idle,
                        &mut self.prev_non_idle,
                        &mut self.pid_mapping,
                        self.use_current_cpu_total,
                        current_instant
                            .duration_since(self.last_collection_time)
                            .as_secs(),
                        self.mem_total_kb,
                        self.page_file_size_kb,
                    )
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Ok(Vec::new())
                }
            } else {
                #[cfg(not(target_os = "linux"))]
                {
                    processes::windows_macos_processes(
                        &self.sys,
                        self.use_current_cpu_total,
                        self.mem_total_kb,
                    )
                }
                #[cfg(target_os = "linux")]
                {
                    Ok(Vec::new())
                }
            } {
                self.data.list_of_processes = Some(process_list);
            }

            if log_enabled!(log::Level::Trace) {
                if let Some(processes) = &self.data.list_of_processes {
                    trace!("processes: {:#?} results", processes.len());
                } else {
                    trace!("Found no processes.");
                }
            }
        }

        // Async if Heim
        let network_data_fut = {
            #[cfg(any(target_os = "windows", target_arch = "aarch64", target_arch = "arm"))]
            {
                network::arm_or_windows_network_data(
                    &self.sys,
                    self.last_collection_time,
                    &mut self.total_rx,
                    &mut self.total_tx,
                    current_instant,
                    self.widgets_to_harvest.use_net,
                )
            }
            #[cfg(not(any(target_os = "windows", target_arch = "aarch64", target_arch = "arm")))]
            {
                network::non_arm_or_windows_network_data(
                    self.last_collection_time,
                    &mut self.total_rx,
                    &mut self.total_tx,
                    current_instant,
                    self.widgets_to_harvest.use_net,
                )
            }
        };
        let mem_data_fut = {
            #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
            {
                mem::arm_mem_data(&self.sys, self.widgets_to_harvest.use_mem)
            }

            #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
            {
                mem::non_arm_mem_data(self.widgets_to_harvest.use_mem)
            }
        };
        let swap_data_fut = {
            #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
            {
                mem::arm_swap_data(&self.sys, self.widgets_to_harvest.use_mem)
            }

            #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
            {
                mem::non_arm_swap_data(self.widgets_to_harvest.use_mem)
            }
        };
        let disk_data_fut = {
            #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
            {
                disks::arm_disk_usage(&self.sys, self.widgets_to_harvest.use_disk)
            }

            #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
            {
                disks::non_arm_disk_usage(self.widgets_to_harvest.use_disk)
            }
        };
        let disk_io_usage_fut = {
            #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
            {
                disks::arm_io_usage(&self.sys, self.widgets_to_harvest.use_disk)
            }

            #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
            {
                disks::non_arm_io_usage(false, self.widgets_to_harvest.use_disk)
            }
        };
        let temp_data_fut = {
            #[cfg(any(not(target_os = "linux"), target_arch = "aarch64", target_arch = "arm"))]
            {
                temperature::arm_and_non_linux_temperature_data(
                    &self.sys,
                    &self.temperature_type,
                    self.widgets_to_harvest.use_temp,
                )
            }

            #[cfg(not(any(
                not(target_os = "linux"),
                target_arch = "aarch64",
                target_arch = "arm"
            )))]
            {
                temperature::linux_temperature_data(
                    &self.temperature_type,
                    self.widgets_to_harvest.use_temp,
                )
            }
        };

        let (net_data, mem_res, swap_res, disk_res, io_res, temp_res) = join!(
            network_data_fut,
            mem_data_fut,
            swap_data_fut,
            disk_data_fut,
            disk_io_usage_fut,
            temp_data_fut
        );

        if let Some(net_data) = net_data {
            self.total_rx = net_data.total_rx;
            self.total_tx = net_data.total_tx;
            self.data.network = Some(net_data);
            if log_enabled!(log::Level::Trace) {
                trace!("Total rx: {:#?}", self.total_rx);
                trace!("Total tx: {:#?}", self.total_tx);
                if let Some(network) = &self.data.network {
                    trace!("network rx: {:#?}", network.rx);
                    trace!("network tx: {:#?}", network.tx);
                } else {
                    trace!("Could not find any networks.");
                }
            }
        }

        if let Ok(memory) = mem_res {
            self.data.memory = memory;
            if log_enabled!(log::Level::Trace) {
                trace!("mem: {:?} results", self.data.memory);
            }
        }

        if let Ok(swap) = swap_res {
            self.data.swap = swap;
            if log_enabled!(log::Level::Trace) {
                trace!("swap: {:?} results", self.data.swap);
            }
        }

        if let Ok(disks) = disk_res {
            self.data.disks = disks;
            if log_enabled!(log::Level::Trace) {
                if let Some(disks) = &self.data.disks {
                    trace!("disks: {:#?} results", disks.len());
                } else {
                    trace!("Could not find any disks.");
                }
            }
        }

        if let Ok(io) = io_res {
            self.data.io = io;
            if log_enabled!(log::Level::Trace) {
                if let Some(io) = &self.data.io {
                    trace!("io: {:#?} results", io.len());
                } else {
                    trace!("Could not find any io results.");
                }
            }
        }

        if let Ok(temp) = temp_res {
            self.data.temperature_sensors = temp;
            if log_enabled!(log::Level::Trace) {
                if let Some(sensors) = &self.data.temperature_sensors {
                    trace!("temp: {:#?} results", sensors.len());
                } else {
                    trace!("Could not find any temp sensors.");
                }
            }
        }

        // Update time
        self.data.last_collection_time = current_instant;
        self.last_collection_time = current_instant;
    }
}
