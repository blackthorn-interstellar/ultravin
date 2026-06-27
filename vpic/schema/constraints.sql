ALTER TABLE ONLY vpic.busfloorconfigtype
    ADD CONSTRAINT ix_busfloorconfigtype UNIQUE (name);

ALTER TABLE ONLY vpic.bustype
    ADD CONSTRAINT ix_bustype UNIQUE (name);

ALTER TABLE ONLY vpic.chargerlevel
    ADD CONSTRAINT ix_chargerlevel UNIQUE (name);

ALTER TABLE ONLY vpic.coolingtype
    ADD CONSTRAINT ix_coolingtype UNIQUE (name);

ALTER TABLE ONLY vpic.custommotorcycletype
    ADD CONSTRAINT ix_custommotorcycletype UNIQUE (name);

ALTER TABLE ONLY vpic.electrificationlevel
    ADD CONSTRAINT ix_electrificationlevel UNIQUE (name);

ALTER TABLE ONLY vpic.enginemodelpattern
    ADD CONSTRAINT ix_enginemodelpattern_keyelement_unique UNIQUE (enginemodelid, elementid);

ALTER TABLE ONLY vpic.motorcyclechassistype
    ADD CONSTRAINT ix_motorcyclechassistype UNIQUE (name);

ALTER TABLE ONLY vpic.motorcyclesuspensiontype
    ADD CONSTRAINT ix_motorcyclesuspensiontype UNIQUE (name);

ALTER TABLE ONLY vpic.errorcode
    ADD CONSTRAINT ix_name UNIQUE (name);

ALTER TABLE ONLY vpic.pattern
    ADD CONSTRAINT ix_pattern_keyelement_unique UNIQUE (vinschemaid, keys, elementid);

ALTER TABLE ONLY vpic.tpms
    ADD CONSTRAINT ix_tpms UNIQUE (name);

ALTER TABLE ONLY vpic.wmi_vinschema
    ADD CONSTRAINT ix_wmi_vinschema UNIQUE (vinschemaid, wmiid, yearfrom);

ALTER TABLE ONLY vpic.adaptivedrivingbeam
    ADD CONSTRAINT pk__adaptive__3214ec0753f971cf PRIMARY KEY (id);

ALTER TABLE ONLY vpic.automaticpedestrainalertingsound
    ADD CONSTRAINT pk__automati__3214ec079a150e03 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.autoreversesystem
    ADD CONSTRAINT pk__autoreve__3214ec07d99b9c79 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.can_aacn
    ADD CONSTRAINT pk__can_aacn__3214ec079657df7c PRIMARY KEY (id);

ALTER TABLE ONLY vpic.daytimerunninglight
    ADD CONSTRAINT pk__daytimer__3214ec07e6e39743 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.dynamicbrakesupport
    ADD CONSTRAINT pk__dynamicb__3214ec07a7d5e8a9 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.edr
    ADD CONSTRAINT pk__edr__3214ec07273a4b03 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.keylessignition
    ADD CONSTRAINT pk__keylessi__3214ec0729e1cccb PRIMARY KEY (id);

ALTER TABLE ONLY vpic.lowerbeamheadlamplightsource
    ADD CONSTRAINT pk__lowerbea__3214ec077a78c7a5 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.pedestrianautomaticemergencybraking
    ADD CONSTRAINT pk__pedestri__3214ec07a638ba23 PRIMARY KEY (id);

ALTER TABLE ONLY vpic.semiautomaticheadlampbeamswitching
    ADD CONSTRAINT pk__semiauto__3214ec071068b17a PRIMARY KEY (id);

ALTER TABLE ONLY vpic.abs
    ADD CONSTRAINT pk_abs PRIMARY KEY (id);

ALTER TABLE ONLY vpic.adaptivecruisecontrol
    ADD CONSTRAINT pk_adaptivecruisecontrol PRIMARY KEY (id);

ALTER TABLE ONLY vpic.airbaglocations
    ADD CONSTRAINT pk_airbaglocations PRIMARY KEY (id);

ALTER TABLE ONLY vpic.airbaglocfront
    ADD CONSTRAINT pk_airbaglocfront PRIMARY KEY (id);

ALTER TABLE ONLY vpic.airbaglocknee
    ADD CONSTRAINT pk_airbaglocknee PRIMARY KEY (id);

ALTER TABLE ONLY vpic.autobrake
    ADD CONSTRAINT pk_autobrake PRIMARY KEY (id);

ALTER TABLE ONLY vpic.axleconfiguration
    ADD CONSTRAINT pk_axleconfiguration PRIMARY KEY (id);

ALTER TABLE ONLY vpic.batterytype
    ADD CONSTRAINT pk_batterytype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.bedtype
    ADD CONSTRAINT pk_bedtype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.blindspotintervention
    ADD CONSTRAINT pk_blindspotintervention PRIMARY KEY (id);

ALTER TABLE ONLY vpic.blindspotmonitoring
    ADD CONSTRAINT pk_blindspotmonitoring PRIMARY KEY (id);

ALTER TABLE ONLY vpic.bodycab
    ADD CONSTRAINT pk_bodycab PRIMARY KEY (id);

ALTER TABLE ONLY vpic.bodystyle
    ADD CONSTRAINT pk_bodystyle PRIMARY KEY (id);

ALTER TABLE ONLY vpic.brakesystem
    ADD CONSTRAINT pk_brakesystem PRIMARY KEY (id);

ALTER TABLE ONLY vpic.busfloorconfigtype
    ADD CONSTRAINT pk_busfloorconfigtype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.bustype
    ADD CONSTRAINT pk_bustype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.chargerlevel
    ADD CONSTRAINT pk_chargerlevel PRIMARY KEY (id);

ALTER TABLE ONLY vpic.combinedbrakingsystem
    ADD CONSTRAINT pk_combinedbrakingsystem PRIMARY KEY (id);

ALTER TABLE ONLY vpic.conversion
    ADD CONSTRAINT pk_conversion PRIMARY KEY (id);

ALTER TABLE ONLY vpic.coolingtype
    ADD CONSTRAINT pk_coolingtype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.country
    ADD CONSTRAINT pk_country PRIMARY KEY (id);

ALTER TABLE ONLY vpic.custommotorcycletype
    ADD CONSTRAINT pk_custommotorcycletype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.decodingoutput
    ADD CONSTRAINT pk_decodingoutput PRIMARY KEY (id);

ALTER TABLE ONLY vpic.defaultvalue
    ADD CONSTRAINT pk_defaultvalue PRIMARY KEY (id);

ALTER TABLE ONLY vpic.defs_body
    ADD CONSTRAINT pk_defs_body PRIMARY KEY (id, from_year, mode);

ALTER TABLE ONLY vpic.defs_make
    ADD CONSTRAINT pk_defs_make PRIMARY KEY (id, from_year);

ALTER TABLE ONLY vpic.defs_model
    ADD CONSTRAINT pk_defs_model PRIMARY KEY (make, id, from_year, mode);

ALTER TABLE ONLY vpic.destinationmarket
    ADD CONSTRAINT pk_destinationmarket PRIMARY KEY (id);

ALTER TABLE ONLY vpic.drivetype
    ADD CONSTRAINT pk_drivetype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.ecs
    ADD CONSTRAINT pk_ecs PRIMARY KEY (id);

ALTER TABLE ONLY vpic.electrificationlevel
    ADD CONSTRAINT pk_electrificationlevel PRIMARY KEY (id);

ALTER TABLE ONLY vpic.element
    ADD CONSTRAINT pk_element PRIMARY KEY (id);

ALTER TABLE ONLY vpic.engineconfiguration
    ADD CONSTRAINT pk_engineconfiguration PRIMARY KEY (id);

ALTER TABLE ONLY vpic.enginemodel
    ADD CONSTRAINT pk_enginemodel PRIMARY KEY (id);

ALTER TABLE ONLY vpic.enginemodelpattern
    ADD CONSTRAINT pk_enginemodelpattern PRIMARY KEY (id);

ALTER TABLE ONLY vpic.entertainmentsystem
    ADD CONSTRAINT pk_entertainmentsystem PRIMARY KEY (id);

ALTER TABLE ONLY vpic.errorcode
    ADD CONSTRAINT pk_errorcode PRIMARY KEY (id);

ALTER TABLE ONLY vpic.evdriveunit
    ADD CONSTRAINT pk_evdriveunit PRIMARY KEY (id);

ALTER TABLE ONLY vpic.forwardcollisionwarning
    ADD CONSTRAINT pk_forwardcollisionwarning PRIMARY KEY (id);

ALTER TABLE ONLY vpic.fueldeliverytype
    ADD CONSTRAINT pk_fueldeliverytype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.fueltankmaterial
    ADD CONSTRAINT pk_fueltankmaterial PRIMARY KEY (id);

ALTER TABLE ONLY vpic.fueltanktype
    ADD CONSTRAINT pk_fueltanktype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.fueltype
    ADD CONSTRAINT pk_fueltype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.grossvehicleweightrating
    ADD CONSTRAINT pk_grossvehicleweightrating PRIMARY KEY (id);

ALTER TABLE ONLY vpic.lanecenteringassistance
    ADD CONSTRAINT pk_lanecenteringassistance PRIMARY KEY (id);

ALTER TABLE ONLY vpic.lanedeparturewarning
    ADD CONSTRAINT pk_lanedeparturewarning PRIMARY KEY (id);

ALTER TABLE ONLY vpic.lanekeepsystem
    ADD CONSTRAINT pk_lanekeepsystem PRIMARY KEY (id);

ALTER TABLE ONLY vpic.make
    ADD CONSTRAINT pk_make PRIMARY KEY (id);

ALTER TABLE ONLY vpic.make_model
    ADD CONSTRAINT pk_make_model PRIMARY KEY (id);

ALTER TABLE ONLY vpic.manufacturer
    ADD CONSTRAINT pk_manufacturer PRIMARY KEY (id);

ALTER TABLE ONLY vpic.manufacturer_make
    ADD CONSTRAINT pk_manufacturer_make PRIMARY KEY (id);

ALTER TABLE ONLY vpic.model
    ADD CONSTRAINT pk_model PRIMARY KEY (id);

ALTER TABLE ONLY vpic.motorcyclechassistype
    ADD CONSTRAINT pk_motorcyclechassistype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.motorcyclesuspensiontype
    ADD CONSTRAINT pk_motorcyclesuspensiontype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.nonlanduse
    ADD CONSTRAINT pk_nonlanduse PRIMARY KEY (id);

ALTER TABLE ONLY vpic.parkassist
    ADD CONSTRAINT pk_parkassist PRIMARY KEY (id);

ALTER TABLE ONLY vpic.pattern
    ADD CONSTRAINT pk_pattern PRIMARY KEY (id);

ALTER TABLE ONLY vpic.pretensioner
    ADD CONSTRAINT pk_pretensioner PRIMARY KEY (id);

ALTER TABLE ONLY vpic.rearautomaticemergencybraking
    ADD CONSTRAINT pk_rearautomaticemergencybraking PRIMARY KEY (id);

ALTER TABLE ONLY vpic.rearcrosstrafficalert
    ADD CONSTRAINT pk_rearcrosstrafficalert PRIMARY KEY (id);

ALTER TABLE ONLY vpic.rearvisibilitycamera
    ADD CONSTRAINT pk_rearvisibilitycamera PRIMARY KEY (id);

ALTER TABLE ONLY vpic.seatbeltsall
    ADD CONSTRAINT pk_seatbeltsall PRIMARY KEY (id);

ALTER TABLE ONLY vpic.steering
    ADD CONSTRAINT pk_steering PRIMARY KEY (id);

ALTER TABLE ONLY vpic.tpms
    ADD CONSTRAINT pk_tpms PRIMARY KEY (id);

ALTER TABLE ONLY vpic.tractioncontrol
    ADD CONSTRAINT pk_tractioncontrol PRIMARY KEY (id);

ALTER TABLE ONLY vpic.trailerbodytype
    ADD CONSTRAINT pk_trailerbodytype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.trailertype
    ADD CONSTRAINT pk_trailertype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.transmission
    ADD CONSTRAINT pk_transmission PRIMARY KEY (id);

ALTER TABLE ONLY vpic.turbo
    ADD CONSTRAINT pk_turbo PRIMARY KEY (id);

ALTER TABLE ONLY vpic.valvetraindesign
    ADD CONSTRAINT pk_valvetraindesign PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vehiclespecpattern
    ADD CONSTRAINT pk_vehicledata_notwmirelated PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vehiclespecschema
    ADD CONSTRAINT pk_vehiclespec PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vehiclespecschema_model
    ADD CONSTRAINT pk_vehiclespecschema_model PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vehiclespecschema_year
    ADD CONSTRAINT pk_vehiclespecschema_year PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vehicletype
    ADD CONSTRAINT pk_vehicletype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vindescriptor
    ADD CONSTRAINT pk_vindescriptor PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vinexception
    ADD CONSTRAINT pk_vinexception PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vinschema
    ADD CONSTRAINT pk_vinschema PRIMARY KEY (id);

ALTER TABLE ONLY vpic.vspecschemapattern
    ADD CONSTRAINT pk_vspecschemapattern PRIMARY KEY (id);

ALTER TABLE ONLY vpic.wheelbasetype
    ADD CONSTRAINT pk_wheelbasetype PRIMARY KEY (id);

ALTER TABLE ONLY vpic.wheeliemitigation
    ADD CONSTRAINT pk_wheeliemitigation PRIMARY KEY (id);

ALTER TABLE ONLY vpic.wmi
    ADD CONSTRAINT pk_wmi PRIMARY KEY (id);

ALTER TABLE ONLY vpic.wmi_make
    ADD CONSTRAINT pk_wmi_make PRIMARY KEY (wmiid, makeid);

ALTER TABLE ONLY vpic.wmi_vinschema
    ADD CONSTRAINT pk_wmi_vinschema PRIMARY KEY (id);

ALTER TABLE ONLY vpic.wmiyearvalidchars
    ADD CONSTRAINT pk_wmiyearvalidchars PRIMARY KEY (id);

ALTER TABLE ONLY vpic.abs
    ADD CONSTRAINT u_absname UNIQUE (name);

ALTER TABLE ONLY vpic.adaptivecruisecontrol
    ADD CONSTRAINT u_adaptivecruisecontrolname UNIQUE (name);

ALTER TABLE ONLY vpic.autobrake
    ADD CONSTRAINT u_aebname UNIQUE (name);

ALTER TABLE ONLY vpic.airbaglocfront
    ADD CONSTRAINT u_airbaglocfront_name UNIQUE (name);

ALTER TABLE ONLY vpic.airbaglocknee
    ADD CONSTRAINT u_airbaglocknee_name UNIQUE (name);

ALTER TABLE ONLY vpic.airbaglocations
    ADD CONSTRAINT u_airbaglocname UNIQUE (name);

ALTER TABLE ONLY vpic.axleconfiguration
    ADD CONSTRAINT u_axleconfiguration UNIQUE (name);

ALTER TABLE ONLY vpic.batterytype
    ADD CONSTRAINT u_batterytypename UNIQUE (name);

ALTER TABLE ONLY vpic.bedtype
    ADD CONSTRAINT u_bedtypename UNIQUE (name);

ALTER TABLE ONLY vpic.blindspotintervention
    ADD CONSTRAINT u_blindspotintervention UNIQUE (name);

ALTER TABLE ONLY vpic.blindspotmonitoring
    ADD CONSTRAINT u_blindspotmonname UNIQUE (name);

ALTER TABLE ONLY vpic.bodycab
    ADD CONSTRAINT u_bodycabtypename UNIQUE (name);

ALTER TABLE ONLY vpic.bodystyle
    ADD CONSTRAINT u_bodyclassname UNIQUE (name);

ALTER TABLE ONLY vpic.brakesystem
    ADD CONSTRAINT u_brakesystemtypename UNIQUE (name);

ALTER TABLE ONLY vpic.combinedbrakingsystem
    ADD CONSTRAINT u_combinedbrakingsystem UNIQUE (name);

ALTER TABLE ONLY vpic.country
    ADD CONSTRAINT u_countryname UNIQUE (name);

ALTER TABLE ONLY vpic.destinationmarket
    ADD CONSTRAINT u_destinationmarketname UNIQUE (name);

ALTER TABLE ONLY vpic.drivetype
    ADD CONSTRAINT u_drivetypename UNIQUE (name);

ALTER TABLE ONLY vpic.engineconfiguration
    ADD CONSTRAINT u_engineconfigurationname UNIQUE (name);

ALTER TABLE ONLY vpic.entertainmentsystem
    ADD CONSTRAINT u_entertainmentsystemname UNIQUE (name);

ALTER TABLE ONLY vpic.ecs
    ADD CONSTRAINT u_escname UNIQUE (name);

ALTER TABLE ONLY vpic.evdriveunit
    ADD CONSTRAINT u_evdriveunitname UNIQUE (name);

ALTER TABLE ONLY vpic.forwardcollisionwarning
    ADD CONSTRAINT u_forwardcollisionwarningname UNIQUE (name);

ALTER TABLE ONLY vpic.fueldeliverytype
    ADD CONSTRAINT u_fuelinjectiontypename UNIQUE (name);

ALTER TABLE ONLY vpic.fueltankmaterial
    ADD CONSTRAINT u_fueltankmaterial UNIQUE (name);

ALTER TABLE ONLY vpic.fueltanktype
    ADD CONSTRAINT u_fueltanktype UNIQUE (name);

ALTER TABLE ONLY vpic.fueltype
    ADD CONSTRAINT u_fueltypename UNIQUE (name);

ALTER TABLE ONLY vpic.grossvehicleweightrating
    ADD CONSTRAINT u_gvwrname UNIQUE (name);

ALTER TABLE ONLY vpic.lanecenteringassistance
    ADD CONSTRAINT u_lanecenteringassistance UNIQUE (name);

ALTER TABLE ONLY vpic.lanedeparturewarning
    ADD CONSTRAINT u_lanedeparturewarningname UNIQUE (name);

ALTER TABLE ONLY vpic.lanekeepsystem
    ADD CONSTRAINT u_lanekeepsystemname UNIQUE (name);

ALTER TABLE ONLY vpic.make
    ADD CONSTRAINT u_makename UNIQUE (name);

ALTER TABLE ONLY vpic.nonlanduse
    ADD CONSTRAINT u_nonlandusename UNIQUE (name);

ALTER TABLE ONLY vpic.parkassist
    ADD CONSTRAINT u_parkassistname UNIQUE (name);

ALTER TABLE ONLY vpic.pretensioner
    ADD CONSTRAINT u_pretensionername UNIQUE (name);

ALTER TABLE ONLY vpic.rearautomaticemergencybraking
    ADD CONSTRAINT u_rearautomaticemergencybraking UNIQUE (name);

ALTER TABLE ONLY vpic.rearvisibilitycamera
    ADD CONSTRAINT u_rearvisibilitycameraname UNIQUE (name);

ALTER TABLE ONLY vpic.seatbeltsall
    ADD CONSTRAINT u_seatbeltsallname UNIQUE (name);

ALTER TABLE ONLY vpic.steering
    ADD CONSTRAINT u_steeringlocationname UNIQUE (name);

ALTER TABLE ONLY vpic.tractioncontrol
    ADD CONSTRAINT u_tractioncontrolname UNIQUE (name);

ALTER TABLE ONLY vpic.trailerbodytype
    ADD CONSTRAINT u_trailerbodytypename UNIQUE (name);

ALTER TABLE ONLY vpic.trailertype
    ADD CONSTRAINT u_trailertypename UNIQUE (name);

ALTER TABLE ONLY vpic.transmission
    ADD CONSTRAINT u_transmissionstylename UNIQUE (name);

ALTER TABLE ONLY vpic.turbo
    ADD CONSTRAINT u_turboname UNIQUE (name);

ALTER TABLE ONLY vpic.valvetraindesign
    ADD CONSTRAINT u_valvetraindesignname UNIQUE (name);

ALTER TABLE ONLY vpic.vehicletype
    ADD CONSTRAINT u_vehicletypename UNIQUE (name);

ALTER TABLE ONLY vpic.wheelbasetype
    ADD CONSTRAINT u_wheelbasetypename UNIQUE (name);

ALTER TABLE ONLY vpic.wheeliemitigation
    ADD CONSTRAINT u_wheeliemitigation UNIQUE (name);

CREATE INDEX ix_element ON vpic.element USING btree (id, name);

CREATE INDEX ix_make ON vpic.make USING btree (name);

CREATE INDEX ix_make_model ON vpic.make_model USING btree (makeid, modelid);

CREATE INDEX ix_make_model_makeid ON vpic.make_model USING btree (makeid);

CREATE INDEX ix_make_model_modelid ON vpic.make_model USING btree (modelid) INCLUDE (makeid);

CREATE INDEX ix_manufacturer ON vpic.manufacturer USING btree (name);

CREATE UNIQUE INDEX ix_manufacturer_make ON vpic.manufacturer_make USING btree (makeid, manufacturerid);

CREATE INDEX ix_pattern ON vpic.pattern USING btree (vinschemaid);

CREATE INDEX ix_vehiclespecpattern ON vpic.vehiclespecpattern USING btree (iskey, elementid, attributeid);

CREATE INDEX ix_vehiclespecpattern_iskey_eid_attrid ON vpic.vehiclespecpattern USING btree (vspecschemapatternid) INCLUDE (iskey);

CREATE INDEX ix_vehiclespecschema_vehicletypeid_makeid ON vpic.vehiclespecschema USING btree (makeid, vehicletypeid) INCLUDE (id);

CREATE INDEX ix_vehiclespecschema_year ON vpic.vehiclespecschema_year USING btree (vehiclespecschemaid, year);

CREATE UNIQUE INDEX ix_vindescriptor_descriptor ON vpic.vindescriptor USING btree (descriptor);

CREATE UNIQUE INDEX ix_wmi ON vpic.wmi USING btree (wmi);

CREATE INDEX ix_wmi_make_makeid ON vpic.wmi_make USING btree (makeid);

CREATE INDEX ix_wmivalidchars ON vpic.wmiyearvalidchars USING btree (wmi);

CREATE INDEX ix_wmiyearvalidchars ON vpic.wmiyearvalidchars USING btree (wmi, year) INCLUDE ("position");

CREATE INDEX "nonclusteredindex-20150710-113712" ON vpic.pattern USING btree (elementid) INCLUDE (attributeid);

CREATE INDEX "nonclusteredindex-20150710-115000" ON vpic.element USING btree (code);

CREATE INDEX "nonclusteredindex-20150710-115058" ON vpic.wmi_vinschema USING btree (wmiid);

CREATE INDEX "nonclusteredindex-20150710-115134" ON vpic.wmi USING btree (manufacturerid);

CREATE INDEX "nonclusteredindex-20150710-115154" ON vpic.wmi USING btree (vehicletypeid);

CREATE INDEX "nonclusteredindex-20150710-115235" ON vpic.wmi_vinschema USING btree (vinschemaid);

CREATE INDEX "nonclusteredindex-20150726-231147" ON vpic.wmi USING btree (wmi, publicavailabilitydate);

CREATE INDEX "nonclusteredindex-20150726-231207" ON vpic.wmi USING btree (wmi);

CREATE INDEX "nonclusteredindex-20160721-081119" ON vpic.pattern USING btree (createdon, updatedon);

CREATE INDEX "nonclusteredindex-20221116-134353" ON vpic.wmi_vinschema USING btree (vinschemaid);

ALTER TABLE ONLY vpic.defaultvalue
    ADD CONSTRAINT fk_defaultvalue_elementid FOREIGN KEY (elementid) REFERENCES vpic.element(id);

ALTER TABLE ONLY vpic.defaultvalue
    ADD CONSTRAINT fk_defaultvalue_vehicletypeid FOREIGN KEY (vehicletypeid) REFERENCES vpic.vehicletype(id);

ALTER TABLE ONLY vpic.enginemodelpattern
    ADD CONSTRAINT fk_enginemodelpattern_enginemodel FOREIGN KEY (enginemodelid) REFERENCES vpic.enginemodel(id);

ALTER TABLE ONLY vpic.make_model
    ADD CONSTRAINT fk_make_model_make FOREIGN KEY (makeid) REFERENCES vpic.make(id);

ALTER TABLE ONLY vpic.make_model
    ADD CONSTRAINT fk_make_model_model FOREIGN KEY (modelid) REFERENCES vpic.model(id);

ALTER TABLE ONLY vpic.pattern
    ADD CONSTRAINT fk_pattern_element FOREIGN KEY (elementid) REFERENCES vpic.element(id);

ALTER TABLE ONLY vpic.pattern
    ADD CONSTRAINT fk_pattern_vinschema FOREIGN KEY (vinschemaid) REFERENCES vpic.vinschema(id);

ALTER TABLE ONLY vpic.vehiclespecpattern
    ADD CONSTRAINT fk_vehicledata_notwmirelated_element FOREIGN KEY (elementid) REFERENCES vpic.element(id);

ALTER TABLE ONLY vpic.vspecschemapattern
    ADD CONSTRAINT fk_vspecschema_vspecpattern_vehiclespecschema FOREIGN KEY (schemaid) REFERENCES vpic.vehiclespecschema(id);

ALTER TABLE ONLY vpic.wmi_make
    ADD CONSTRAINT fk_wmi_make_make FOREIGN KEY (makeid) REFERENCES vpic.make(id);

ALTER TABLE ONLY vpic.wmi_make
    ADD CONSTRAINT fk_wmi_make_wmi FOREIGN KEY (wmiid) REFERENCES vpic.wmi(id);

ALTER TABLE ONLY vpic.wmi
    ADD CONSTRAINT fk_wmi_manufacturer FOREIGN KEY (manufacturerid) REFERENCES vpic.manufacturer(id);

ALTER TABLE ONLY vpic.wmi
    ADD CONSTRAINT fk_wmi_vehicletype FOREIGN KEY (vehicletypeid) REFERENCES vpic.vehicletype(id);

ALTER TABLE ONLY vpic.wmi_vinschema
    ADD CONSTRAINT fk_wmi_vinschema_vinschema FOREIGN KEY (vinschemaid) REFERENCES vpic.vinschema(id);

ALTER TABLE ONLY vpic.wmi_vinschema
    ADD CONSTRAINT fk_wmi_vinschema_wmi FOREIGN KEY (wmiid) REFERENCES vpic.wmi(id);


-- Completed on 2026-06-11 09:38:51

-- PostgreSQL database dump complete

\unrestrict dO69fbNW2dCkiN9wsDnBgbOQJIqxvSuStqxejkwQIanBEMdLpgWKfnYrXjDFpiZ
